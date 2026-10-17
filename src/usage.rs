use std::fs::File;
use std::io::Read;
use std::path::Path;

/// One row from a GitHub detailed usage report.
///
/// Amounts stay as `f64` and are only rounded when a report prints them.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageRow {
    pub sku: String,
    pub quantity: f64,
    pub unit_type: String,
    pub net_amount: f64,
    pub gross_amount: f64,
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
    InvalidCsv,
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
            UsageErrorKind::InvalidCsv => write!(f, "line {}: invalid CSV", self.line),
        }
    }
}

impl std::error::Error for UsageError {}

const EMPTY_WORKFLOW: &str = "(no workflow)";

const COLUMNS: [&str; 6] = [
    "sku",
    "quantity",
    "unit_type",
    "net_amount",
    "gross_amount",
    "workflow_path",
];

pub fn read_usage(path: &Path) -> Result<Vec<UsageRow>, UsageError> {
    let file = File::open(path).map_err(|_| UsageError {
        line: 0,
        kind: UsageErrorKind::Unreadable,
    })?;
    read_reader(file)
}

fn read_reader<R: Read>(reader: R) -> Result<Vec<UsageRow>, UsageError> {
    let mut csv_reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .trim(csv::Trim::All)
        .from_reader(reader);

    let headers = csv_reader.headers().map_err(csv_error)?.clone();
    let header_line = record_line(&headers);
    let indexes = column_indexes(&headers, header_line)?;

    let mut rows = Vec::new();
    for result in csv_reader.records() {
        let record = result.map_err(csv_error)?;
        let line = record_line(&record);
        rows.push(parse_row(&record, &indexes, line)?);
    }
    Ok(rows)
}

struct ColumnIndexes {
    sku: usize,
    quantity: usize,
    unit_type: usize,
    net_amount: usize,
    gross_amount: usize,
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
        sku: index(COLUMNS[0])?,
        quantity: index(COLUMNS[1])?,
        unit_type: index(COLUMNS[2])?,
        net_amount: index(COLUMNS[3])?,
        gross_amount: index(COLUMNS[4])?,
        workflow_path: index(COLUMNS[5])?,
    })
}

fn parse_row(
    record: &csv::StringRecord,
    indexes: &ColumnIndexes,
    line: u64,
) -> Result<UsageRow, UsageError> {
    Ok(UsageRow {
        sku: field(record, indexes.sku, line)?.to_string(),
        quantity: parse_number(field(record, indexes.quantity, line)?, line, "quantity")?,
        unit_type: field(record, indexes.unit_type, line)?.to_string(),
        net_amount: parse_number(field(record, indexes.net_amount, line)?, line, "net_amount")?,
        gross_amount: parse_number(
            field(record, indexes.gross_amount, line)?,
            line,
            "gross_amount",
        )?,
        workflow_path: normalize_workflow(field(record, indexes.workflow_path, line)?),
    })
}

fn field(record: &csv::StringRecord, index: usize, line: u64) -> Result<&str, UsageError> {
    record.get(index).ok_or(UsageError {
        line,
        kind: UsageErrorKind::InvalidCsv,
    })
}

fn parse_number(raw: &str, line: u64, column: &'static str) -> Result<f64, UsageError> {
    let value = raw.parse::<f64>().ok().filter(|value| value.is_finite());
    value.ok_or(UsageError {
        line,
        kind: UsageErrorKind::NotANumber(column),
    })
}

fn normalize_workflow(raw: &str) -> String {
    if raw.is_empty() {
        EMPTY_WORKFLOW.to_string()
    } else {
        raw.to_string()
    }
}

fn record_line(record: &csv::StringRecord) -> u64 {
    record
        .position()
        .map(|position| position.line())
        .unwrap_or(1)
}

fn csv_error(err: csv::Error) -> UsageError {
    UsageError {
        line: err.position().map(|position| position.line()).unwrap_or(1),
        kind: UsageErrorKind::InvalidCsv,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "sku,quantity,unit_type,net_amount,gross_amount,workflow_path";

    fn parse(csv_text: &str) -> Result<Vec<UsageRow>, UsageError> {
        read_reader(csv_text.as_bytes())
    }

    #[test]
    fn column_order_does_not_matter() {
        let canonical = "\
sku,quantity,unit_type,net_amount,gross_amount,workflow_path
actions_macos,10,minutes,2.5,3,a.yml
actions_linux,4,minutes,1.25,1.5,b.yml
";
        let shuffled = "\
workflow_path,gross_amount,extra,net_amount,Unit_Type,quantity,SKU
a.yml,3,ignore-me,2.5,minutes,10,actions_macos
b.yml,1.5,also-ignored,1.25,minutes,4,actions_linux
";
        let left = parse(canonical).unwrap();
        let right = parse(shuffled).unwrap();
        assert_eq!(left, right);
        assert_eq!(left[0].net_amount, "2.5".parse::<f64>().unwrap());
        assert_eq!(left[1].gross_amount, "1.5".parse::<f64>().unwrap());
        assert!(!left[0].sku.contains("ignore"));
    }

    #[test]
    fn missing_column_names_the_column_and_line() {
        let csv_text = "\
sku,quantity,unit_type,gross_amount,workflow_path
actions_linux,1,minutes,1,SECRET_CELL_VALUE
";
        let err = parse(csv_text).unwrap_err();
        assert_eq!(err.line, 1);
        assert_eq!(err.kind, UsageErrorKind::MissingColumn("net_amount"));
        let message = err.to_string();
        assert_eq!(message, "line 1: missing column net_amount");
        assert!(!message.contains("SECRET_CELL_VALUE"));
    }

    #[test]
    fn non_numeric_value_reports_line_without_cell() {
        let cases = [
            (
                "actions_linux,NOT_A_NUMBER,minutes,1,1,a.yml",
                2,
                "quantity",
                "NOT_A_NUMBER",
            ),
            (
                "actions_linux,1,minutes,12abc34,1,a.yml",
                2,
                "net_amount",
                "12abc34",
            ),
            (
                "actions_linux,1,minutes,1,NaN,a.yml",
                2,
                "gross_amount",
                "NaN",
            ),
        ];
        for (row, line, column, token) in cases {
            let csv_text = format!("{HEADER}\n{row}\n");
            let err = parse(&csv_text).unwrap_err();
            assert_eq!(err.line, line, "{column}");
            assert_eq!(err.kind, UsageErrorKind::NotANumber(column));
            let message = err.to_string();
            assert_eq!(message, format!("line {line}: {column} is not a number"));
            assert!(!message.contains(token));
        }

        let later = format!("{HEADER}\nok,1,minutes,1,1,a.yml\nok,2,minutes,2,2,a.yml\nok,BROKEN,minutes,3,3,a.yml\n");
        let err = parse(&later).unwrap_err();
        assert_eq!(err.line, 4);
        assert_eq!(err.kind, UsageErrorKind::NotANumber("quantity"));
        assert!(!err.to_string().contains("BROKEN"));
    }

    #[test]
    fn utf8_bom_is_accepted() {
        let csv_text =
            format!("{HEADER}\nactions_linux,1.5,minutes,0.25,0.5,.github/workflows/ci.yml\n");
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend(csv_text.as_bytes());
        let rows = read_reader(bytes.as_slice()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].sku, "actions_linux");
        assert_eq!(rows[0].quantity, 1.5);
        assert_eq!(rows[0].workflow_path, ".github/workflows/ci.yml");
    }

    #[test]
    fn empty_workflow_path_uses_placeholder() {
        let csv_text = format!(
            "{HEADER}\nactions_storage,0.5,gigabyte-hours,0,0,\nactions_linux,1,minutes,1,1,   \n"
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
        assert_eq!(rows[0].sku, "actions_macos");
        assert_eq!(rows[0].quantity, 118.0);
        assert_eq!(rows[0].net_amount, "7.316".parse::<f64>().unwrap());
        assert_eq!(rows[0].gross_amount, rows[0].net_amount);
        assert_eq!(rows[0].workflow_path, ".github/workflows/lint.yml");
    }
}
