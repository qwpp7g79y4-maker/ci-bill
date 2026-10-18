#[cfg(test)]
use std::io::Read;
use std::path::Path;

use crate::money::{DecimalError, Micros};

/// One row from a GitHub detailed usage report.
///
/// Money and quantity are [`Micros`]. They are summed as integers and rounded
/// only when a report prints them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRow {
    /// 1-based physical line in the CSV file. The header is line 1.
    pub line: u64,
    pub date: String,
    pub product: String,
    pub sku: String,
    pub quantity: Micros,
    pub unit_type: String,
    pub applied_cost_per_quantity: Micros,
    pub gross: Micros,
    pub discount: Micros,
    pub net: Micros,
    pub username: String,
    pub organization: String,
    pub repository: String,
    pub workflow_path: String,
}

/// An input error while reading a usage report. Exit code 2.
///
/// Messages name the line and the column. They never include cell text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageError {
    pub line: u64,
    pub kind: UsageErrorKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsageErrorKind {
    /// The file could not be opened. `line` is 0 because no row was read.
    Unreadable,
    MissingColumn(&'static str),
    NotANumber(&'static str),
    TooManyDecimals(&'static str),
    InvalidCsv,
    /// A running total overflowed `i64` micros. `line` is the row that caused
    /// it, or 0 for a total summed across the whole report.
    AmountOverflow(&'static str),
}

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            UsageErrorKind::Unreadable => write!(f, "line {}: cannot read file", self.line),
            UsageErrorKind::MissingColumn(column) => {
                write!(f, "line {}: missing column {column}", self.line)
            }
            UsageErrorKind::NotANumber(column) => {
                write!(f, "line {}: {column} is not a number", self.line)
            }
            UsageErrorKind::TooManyDecimals(column) => {
                write!(
                    f,
                    "line {}: {column} has more than 6 decimal places",
                    self.line
                )
            }
            UsageErrorKind::InvalidCsv => write!(f, "line {}: invalid CSV", self.line),
            UsageErrorKind::AmountOverflow(column) => {
                write!(f, "line {}: {column} total is out of range", self.line)
            }
        }
    }
}

impl std::error::Error for UsageError {}

const EMPTY_WORKFLOW: &str = "(no workflow)";

const COLUMNS: [&str; 13] = [
    "date",
    "product",
    "sku",
    "quantity",
    "unit_type",
    "applied_cost_per_quantity",
    "gross_amount",
    "discount_amount",
    "net_amount",
    "username",
    "organization",
    "repository",
    "workflow_path",
];

pub fn read_usage(path: &Path) -> Result<Vec<UsageRow>, UsageError> {
    let bytes = std::fs::read(path).map_err(|_| UsageError {
        line: 0,
        kind: UsageErrorKind::Unreadable,
    })?;
    parse_bytes(&bytes)
}

#[cfg(test)]
fn read_reader<R: Read>(mut reader: R) -> Result<Vec<UsageRow>, UsageError> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).map_err(|_| UsageError {
        line: 0,
        kind: UsageErrorKind::Unreadable,
    })?;
    parse_bytes(&bytes)
}

fn parse_bytes(bytes: &[u8]) -> Result<Vec<UsageRow>, UsageError> {
    let mut csv_reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .trim(csv::Trim::All)
        .from_reader(bytes);

    let headers = csv_reader
        .headers()
        .map_err(|err| csv_error(err, bytes))?
        .clone();
    let header_line = record_line(&headers, bytes);
    let indexes = column_indexes(&headers, header_line)?;

    let mut rows = Vec::new();
    for result in csv_reader.records() {
        let record = result.map_err(|err| csv_error(err, bytes))?;
        let line = record_line(&record, bytes);
        rows.push(parse_row(&record, &indexes, line)?);
    }
    Ok(rows)
}

struct ColumnIndexes {
    date: usize,
    product: usize,
    sku: usize,
    quantity: usize,
    unit_type: usize,
    applied_cost_per_quantity: usize,
    gross_amount: usize,
    discount_amount: usize,
    net_amount: usize,
    username: usize,
    organization: usize,
    repository: usize,
    workflow_path: usize,
}

fn column_indexes(headers: &csv::StringRecord, line: u64) -> Result<ColumnIndexes, UsageError> {
    let index = |name| {
        headers
            .iter()
            .position(|header| header.eq_ignore_ascii_case(name))
            .ok_or(UsageError {
                line,
                kind: UsageErrorKind::MissingColumn(name),
            })
    };
    Ok(ColumnIndexes {
        date: index(COLUMNS[0])?,
        product: index(COLUMNS[1])?,
        sku: index(COLUMNS[2])?,
        quantity: index(COLUMNS[3])?,
        unit_type: index(COLUMNS[4])?,
        applied_cost_per_quantity: index(COLUMNS[5])?,
        gross_amount: index(COLUMNS[6])?,
        discount_amount: index(COLUMNS[7])?,
        net_amount: index(COLUMNS[8])?,
        username: index(COLUMNS[9])?,
        organization: index(COLUMNS[10])?,
        repository: index(COLUMNS[11])?,
        workflow_path: index(COLUMNS[12])?,
    })
}

fn parse_row(
    record: &csv::StringRecord,
    indexes: &ColumnIndexes,
    line: u64,
) -> Result<UsageRow, UsageError> {
    Ok(UsageRow {
        line,
        date: field(record, indexes.date, line)?.to_string(),
        product: field(record, indexes.product, line)?.to_string(),
        sku: field(record, indexes.sku, line)?.to_string(),
        quantity: parse_decimal(field(record, indexes.quantity, line)?, line, "quantity")?,
        unit_type: field(record, indexes.unit_type, line)?.to_string(),
        applied_cost_per_quantity: parse_decimal(
            field(record, indexes.applied_cost_per_quantity, line)?,
            line,
            "applied_cost_per_quantity",
        )?,
        gross: parse_decimal(
            field(record, indexes.gross_amount, line)?,
            line,
            "gross_amount",
        )?,
        discount: parse_decimal(
            field(record, indexes.discount_amount, line)?,
            line,
            "discount_amount",
        )?,
        net: parse_decimal(field(record, indexes.net_amount, line)?, line, "net_amount")?,
        username: field(record, indexes.username, line)?.to_string(),
        organization: field(record, indexes.organization, line)?.to_string(),
        repository: field(record, indexes.repository, line)?.to_string(),
        workflow_path: normalize_workflow(field(record, indexes.workflow_path, line)?),
    })
}

fn field(record: &csv::StringRecord, index: usize, line: u64) -> Result<&str, UsageError> {
    record.get(index).ok_or(UsageError {
        line,
        kind: UsageErrorKind::InvalidCsv,
    })
}

fn parse_decimal(raw: &str, line: u64, column: &'static str) -> Result<Micros, UsageError> {
    match Micros::parse(raw) {
        Ok(value) => Ok(value),
        Err(DecimalError::TooManyDecimals) => Err(UsageError {
            line,
            kind: UsageErrorKind::TooManyDecimals(column),
        }),
        Err(DecimalError::NotANumber) => Err(UsageError {
            line,
            kind: UsageErrorKind::NotANumber(column),
        }),
    }
}

fn normalize_workflow(raw: &str) -> String {
    if raw.is_empty() {
        EMPTY_WORKFLOW.to_string()
    } else {
        raw.to_string()
    }
}

fn record_line(record: &csv::StringRecord, bytes: &[u8]) -> u64 {
    record
        .position()
        .map(|position| physical_line(position.line(), position.byte(), bytes))
        .unwrap_or(1)
}

/// Line number of a record, counting a `\r\n` row as one line.
///
/// The reader stores the line from before it consumes the `\n` of a `\r\n`
/// terminator. That `\n` is still the byte at `offset`, so the stored line is
/// one short. A `\n` terminator is already included in the stored line.
fn physical_line(line: u64, offset: u64, bytes: &[u8]) -> u64 {
    let Some(index) = usize::try_from(offset).ok() else {
        return line;
    };
    if index > 0 && bytes.get(index) == Some(&b'\n') && bytes.get(index - 1) == Some(&b'\r') {
        line.saturating_add(1)
    } else {
        line
    }
}

fn csv_error(err: csv::Error, bytes: &[u8]) -> UsageError {
    let line = err
        .position()
        .map(|position| physical_line(position.line(), position.byte(), bytes))
        .unwrap_or(1);
    UsageError {
        line,
        kind: UsageErrorKind::InvalidCsv,
    }
}

/// `true` when the product column is GitHub Actions, ignoring ASCII case.
pub fn is_actions_product(product: &str) -> bool {
    product.eq_ignore_ascii_case("actions")
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "date,product,sku,quantity,unit_type,applied_cost_per_quantity,gross_amount,discount_amount,net_amount,username,organization,repository,workflow_path";

    fn parse(csv_text: &str) -> Result<Vec<UsageRow>, UsageError> {
        read_reader(csv_text.as_bytes())
    }

    fn usd(text: &str) -> Micros {
        Micros::parse(text).unwrap()
    }

    #[test]
    fn column_order_case_and_crlf_do_not_matter() {
        let canonical = "\
date,product,sku,quantity,unit_type,applied_cost_per_quantity,gross_amount,discount_amount,net_amount,username,organization,repository,workflow_path
2026-09-01,actions,actions_macos,10,minutes,0.062,3,0.5,2.5,ada,example-org,example-org/sample-app,a.yml
2026-09-02,Actions,actions_linux,4,Minutes,0.006,1.5,0,1.25,ada,example-org,example-org/sample-app,b.yml
";
        let shuffled = "\
workflow_path,GROSS_AMOUNT,extra,net_amount,Unit_Type,quantity,SKU,Repository,Date,PRODUCT,username,organization,discount_amount,applied_cost_per_quantity\r\n\
a.yml,3,ignore-me,2.5,minutes,10,actions_macos,example-org/sample-app,2026-09-01,actions,ada,example-org,0.5,0.062\r\n\
b.yml,1.5,also-ignored,1.25,Minutes,4,actions_linux,example-org/sample-app,2026-09-02,Actions,ada,example-org,0,0.006\r\n";
        let left = parse(canonical).unwrap();
        let right = parse(shuffled).unwrap();
        assert_eq!(left, right);
        assert_eq!(left[0].net, usd("2.5"));
        assert_eq!(left[0].gross, usd("3"));
        assert_eq!(left[0].discount, usd("0.5"));
        assert_eq!(left[0].applied_cost_per_quantity, usd("0.062"));
        assert_eq!(left[0].quantity, usd("10"));
        assert_eq!(left[0].date, "2026-09-01");
        assert_eq!(left[0].product, "actions");
        assert_eq!(left[0].username, "ada");
        assert_eq!(left[0].organization, "example-org");
        assert_eq!(left[0].repository, "example-org/sample-app");
        assert_eq!(left[1].gross, usd("1.5"));
        assert!(!left[0].sku.contains("ignore"));
    }

    #[test]
    fn missing_column_names_the_column_and_line() {
        let csv_text = "\
date,product,sku,quantity,unit_type,applied_cost_per_quantity,gross_amount,discount_amount,username,organization,repository,workflow_path
2026-09-01,actions,actions_linux,1,minutes,0.006,1,0,ada,example-org,example-org/app,SECRET_CELL_VALUE
";
        let err = parse(csv_text).unwrap_err();
        assert_eq!(err.line, 1);
        assert_eq!(err.kind, UsageErrorKind::MissingColumn("net_amount"));
        let message = err.to_string();
        assert_eq!(message, "line 1: missing column net_amount");
        assert!(!message.contains("SECRET_CELL_VALUE"));
    }

    #[test]
    fn each_required_column_is_named_when_missing() {
        for column in COLUMNS {
            let headers = COLUMNS
                .iter()
                .copied()
                .filter(|name| *name != column)
                .collect::<Vec<_>>()
                .join(",");
            let csv_text = format!("{headers}\nkeep-secret\n");
            let err = parse(&csv_text).unwrap_err();
            assert_eq!(err.kind, UsageErrorKind::MissingColumn(column), "{column}");
            assert!(!err.to_string().contains("keep-secret"));
        }
    }

    #[test]
    fn non_numeric_value_reports_line_without_cell() {
        let cases = [
            ("quantity", "NOT_A_NUMBER"),
            ("applied_cost_per_quantity", "NaN"),
            ("gross_amount", "inf"),
            ("discount_amount", "-Infinity"),
            ("net_amount", "12abc34"),
        ];
        for (column, token) in cases {
            let mut fields = vec![
                "2026-09-01",
                "actions",
                "actions_linux",
                "1",
                "minutes",
                "0.006",
                "1",
                "0",
                "1",
                "ada",
                "example-org",
                "example-org/app",
                "a.yml",
            ];
            let index = match column {
                "quantity" => 3,
                "applied_cost_per_quantity" => 5,
                "gross_amount" => 6,
                "discount_amount" => 7,
                "net_amount" => 8,
                _ => panic!("column"),
            };
            fields[index] = token;
            let csv_text = format!("{HEADER}\n{}\n", fields.join(","));
            let err = parse(&csv_text).unwrap_err();
            assert_eq!(err.line, 2, "{column}");
            assert_eq!(err.kind, UsageErrorKind::NotANumber(column));
            let message = err.to_string();
            assert_eq!(message, format!("line 2: {column} is not a number"));
            assert!(!message.contains(token));
        }

        let later = format!(
            "{HEADER}\n\
2026-09-01,actions,ok,1,minutes,1,1,0,1,ada,org,org/app,a.yml\n\
2026-09-02,actions,ok,2,minutes,1,2,0,2,ada,org,org/app,a.yml\n\
2026-09-03,actions,ok,BROKEN,minutes,1,3,0,3,ada,org,org/app,a.yml\n"
        );
        let err = parse(&later).unwrap_err();
        assert_eq!(err.line, 4);
        assert_eq!(err.kind, UsageErrorKind::NotANumber("quantity"));
        assert!(!err.to_string().contains("BROKEN"));
    }

    #[test]
    fn seven_decimals_report_the_line_without_the_cell() {
        let tokens = [
            ("quantity", "1.2345678"),
            ("applied_cost_per_quantity", "0.0000001"),
            ("gross_amount", "1e-7"),
            ("discount_amount", "-0.0000001"),
            ("net_amount", "12345678e-7"),
        ];
        for (column, token) in tokens {
            let mut fields = vec![
                "2026-09-01",
                "actions",
                "actions_linux",
                "1",
                "minutes",
                "0.000001",
                "1.234567",
                "0",
                "1.234567",
                "ada",
                "example-org",
                "example-org/app",
                "a.yml",
            ];
            let index = match column {
                "quantity" => 3,
                "applied_cost_per_quantity" => 5,
                "gross_amount" => 6,
                "discount_amount" => 7,
                "net_amount" => 8,
                _ => panic!("column"),
            };
            fields[index] = token;
            let csv_text = format!("{HEADER}\n{}\n{}\n", fields.join(","), fields.join(","));
            let err = parse(&csv_text).unwrap_err();
            assert_eq!(err.line, 2, "{column} {token}");
            assert_eq!(err.kind, UsageErrorKind::TooManyDecimals(column));
            let message = err.to_string();
            assert_eq!(
                message,
                format!("line 2: {column} has more than 6 decimal places")
            );
            assert!(!message.contains(token));
            assert!(!message.contains("1.2345678"));
        }
    }

    #[test]
    fn crlf_decimal_error_uses_the_physical_line() {
        let good = "2026-09-01,actions,actions_linux,1,minutes,0.000001,1.234567,0,1.234567,ada,example-org,example-org/app,a.yml";
        let bad = "2026-09-01,actions,actions_linux,1.2345678,minutes,0.000001,1.234567,0,1.234567,ada,example-org,example-org/app,a.yml";
        let first = format!("{HEADER}\r\n{bad}\r\n");
        let err = parse(&first).unwrap_err();
        assert_eq!(err.line, 2);
        assert_eq!(err.kind, UsageErrorKind::TooManyDecimals("quantity"));
        assert!(!err.to_string().contains("1.2345678"));

        let second = format!("{HEADER}\r\n{good}\r\n{bad}\r\n");
        let err = parse(&second).unwrap_err();
        assert_eq!(err.line, 3);
        assert_eq!(
            err.to_string(),
            "line 3: quantity has more than 6 decimal places"
        );
        assert!(!err.to_string().contains("1.2345678"));
    }

    #[test]
    fn empty_numeric_cells_are_not_numbers_and_empty_text_is_kept() {
        let csv_text = format!(
            "{HEADER}\n\
2026-09-01,actions,actions_linux,,minutes,0.006,1,0,1,ada,example-org,example-org/app,a.yml\n"
        );
        let err = parse(&csv_text).unwrap_err();
        assert_eq!(err.kind, UsageErrorKind::NotANumber("quantity"));
        assert_eq!(err.to_string(), "line 2: quantity is not a number");

        for column in [
            "applied_cost_per_quantity",
            "gross_amount",
            "discount_amount",
            "net_amount",
        ] {
            let mut fields = [
                "2026-09-01",
                "actions",
                "actions_linux",
                "1",
                "minutes",
                "0.006",
                "1",
                "0",
                "1",
                "ada",
                "org",
                "org/app",
                "a.yml",
            ];
            let index = match column {
                "applied_cost_per_quantity" => 5,
                "gross_amount" => 6,
                "discount_amount" => 7,
                "net_amount" => 8,
                _ => panic!("column"),
            };
            fields[index] = "";
            let csv_text = format!("{HEADER}\n{}\n", fields.join(","));
            let err = parse(&csv_text).unwrap_err();
            assert_eq!(err.kind, UsageErrorKind::NotANumber(column), "{column}");
        }

        let blanks = format!(
            "{HEADER}\n\
, ,actions_storage,0.5,gigabyte-hours,0.000336,0.000168,0.000168,0,,, ,   \n\
2026-09-30,ACTIONS,actions_linux,1,MINUTES,0.006,1,0,1, ,Example-Org,example-org/other,ci.yml\n"
        );
        let rows = parse(&blanks).unwrap();
        assert_eq!(rows[0].date, "");
        assert_eq!(rows[0].product, "");
        assert_eq!(rows[0].username, "");
        assert_eq!(rows[0].organization, "");
        assert_eq!(rows[0].repository, "");
        assert_eq!(rows[0].workflow_path, "(no workflow)");
        assert_eq!(rows[0].quantity, usd("0.5"));
        assert_eq!(rows[0].net, Micros::ZERO);
        assert_eq!(rows[0].discount, usd("0.000168"));
        assert_eq!(rows[1].product, "ACTIONS");
        assert_eq!(rows[1].username, "");
        assert_eq!(rows[1].repository, "example-org/other");
        assert_eq!(rows[1].workflow_path, "ci.yml");
        assert!(is_actions_product(&rows[1].product));
        assert!(!is_actions_product(&rows[0].product));
        assert!(!is_actions_product("copilot"));
        assert!(!is_actions_product("COPILOT"));
        assert!(is_actions_product("Actions"));
    }

    #[test]
    fn utf8_bom_is_accepted() {
        let csv_text = format!(
            "{HEADER}\n\
2026-09-01,actions,actions_linux,1.5,minutes,0.006,0.5,0.25,0.25,ada,example-org,example-org/app,.github/workflows/ci.yml\n"
        );
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend(csv_text.as_bytes());
        let rows = read_reader(bytes.as_slice()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].sku, "actions_linux");
        assert_eq!(rows[0].quantity, usd("1.5"));
        assert_eq!(rows[0].net, usd("0.25"));
        assert_eq!(rows[0].workflow_path, ".github/workflows/ci.yml");
    }

    #[test]
    fn empty_workflow_path_uses_placeholder() {
        let csv_text = format!(
            "{HEADER}\n\
2026-09-01,actions,actions_storage,0.5,gigabyte-hours,0.000336,0,0,0,ada,example-org,example-org/app,\n\
2026-09-01,actions,actions_linux,1,minutes,0.006,1,0,1,ada,example-org,example-org/app,   \n"
        );
        let rows = parse(&csv_text).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].workflow_path, "(no workflow)");
        assert_eq!(rows[1].workflow_path, "(no workflow)");
        assert_eq!(rows[0].unit_type, "gigabyte-hours");
    }

    #[test]
    fn unreadable_file_reports_line_zero() {
        let err = read_usage(Path::new("/no/such/ci-bill-usage.csv")).unwrap_err();
        assert_eq!(err.line, 0);
        assert_eq!(err.kind, UsageErrorKind::Unreadable);
        assert_eq!(err.to_string(), "line 0: cannot read file");
    }

    #[test]
    fn sample_usage_report_parses() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/usage-sample.csv");
        let rows = read_usage(&path).unwrap();
        assert_eq!(rows.len(), 150);
        assert_eq!(
            rows.iter()
                .filter(|row| row.workflow_path == "(no workflow)")
                .count(),
            30
        );
        assert_eq!(rows[0].date, "2026-09-01");
        assert_eq!(rows[0].product, "actions");
        assert_eq!(rows[0].sku, "actions_macos");
        assert_eq!(rows[0].quantity, usd("118"));
        assert_eq!(rows[0].unit_type, "minutes");
        assert_eq!(rows[0].applied_cost_per_quantity, usd("0.062"));
        assert_eq!(rows[0].gross, usd("7.316"));
        assert_eq!(rows[0].discount, Micros::ZERO);
        assert_eq!(rows[0].net, usd("7.316"));
        assert_eq!(rows[0].username, "sample-user");
        assert_eq!(rows[0].organization, "example-org");
        assert_eq!(rows[0].repository, "example-org/sample-app");
        assert_eq!(rows[0].workflow_path, ".github/workflows/lint.yml");
        assert_eq!(rows.last().unwrap().date, "2026-09-30");

        let storage = &rows[4];
        assert_eq!(storage.workflow_path, "(no workflow)");
        assert_eq!(storage.sku, "actions_storage");
        assert_eq!(storage.quantity, usd("0.5"));
        assert_eq!(storage.unit_type, "gigabyte-hours");
        assert_eq!(storage.applied_cost_per_quantity, usd("0.000336"));
        assert_eq!(storage.gross, usd("0.000168"));
        assert_eq!(storage.discount, usd("0.000168"));
        assert_eq!(storage.net, Micros::ZERO);
        assert_eq!(storage.username, "");
        assert!(is_actions_product(&storage.product));
    }
}
