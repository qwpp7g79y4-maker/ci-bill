/// Exact decimal stored as an integer count of millionths (10^-6).
///
/// CSV amounts and quantities are parsed from decimal text. Nothing in this
/// type uses floating point. Half-up rounding happens only in the display
/// helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct Micros(pub i64);

/// Why a decimal string was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecimalError {
    /// Empty, text, NaN, infinity, exponent junk, or a magnitude that does not fit.
    NotANumber,
    /// The value is not an integer number of millionths, so it would need a 7th decimal.
    TooManyDecimals,
}

impl Micros {
    pub const ZERO: Self = Self(0);
    /// 1.0 = 1_000_000.
    pub const SCALE: i64 = 1_000_000;

    /// Parse `[-+]digits[.digits][e[-+]digits]`.
    ///
    /// A value that is not an exact multiple of 10^-6 is `TooManyDecimals`.
    /// Trailing zeros are kept only when the value still fits in 6 decimal places
    /// (`1.2300000` is 1.23; `1.2345678` is rejected).
    pub fn parse(raw: &str) -> Result<Self, DecimalError> {
        let text = raw.trim();
        if text.is_empty() {
            return Err(DecimalError::NotANumber);
        }
        let (negative, unsigned) = match text.as_bytes()[0] {
            b'+' => (false, &text[1..]),
            b'-' => (true, &text[1..]),
            _ => (false, text),
        };
        if unsigned.is_empty() {
            return Err(DecimalError::NotANumber);
        }
        let (coefficient, exponent) = split_exponent(unsigned)?;
        let (significand, frac_len) = parse_coefficient(coefficient)?;
        // value = significand * 10^(exponent - frac_len)
        // micros = value * 10^6 = significand * 10^(exponent - frac_len + 6)
        let power = exponent
            .checked_sub(frac_len)
            .and_then(|power| power.checked_add(6))
            .ok_or(DecimalError::NotANumber)?;
        let scaled = scale_pow10(significand, power)?;
        let signed = if negative { -scaled } else { scaled };
        i64::try_from(signed)
            .map(Self)
            .map_err(|_| DecimalError::NotANumber)
    }

    /// `44.232000`. Always 6 digits after the decimal point.
    pub fn to_decimal_string(self) -> String {
        format_fixed(i128::from(self.0), 6)
    }

    /// Half-up, away from zero, to cents. Display only.
    pub fn format_cents(self) -> String {
        let neg = self.0 < 0;
        let abs = self.0.unsigned_abs();
        let mut cents = abs / 10_000;
        let rem = abs % 10_000;
        if rem >= 5_000 {
            cents += 1;
        }
        let sign = if neg && cents != 0 { "-" } else { "" };
        format!("{sign}{}.{:02}", cents / 100, cents % 100)
    }

    /// Up to 3 decimal places, half-up, trailing zeros removed. Used for minutes.
    pub fn format_quantity(self) -> String {
        let neg = self.0 < 0;
        let abs = self.0.unsigned_abs();
        let mut thousandths = abs / 1_000;
        let rem = abs % 1_000;
        if rem >= 500 {
            thousandths += 1;
        }
        let sign = if neg && thousandths != 0 { "-" } else { "" };
        let whole = thousandths / 1_000;
        let frac = (thousandths % 1_000) as u32;
        if frac == 0 {
            return format!("{sign}{whole}");
        }
        let digits = format!("{frac:03}");
        let trimmed = digits.trim_end_matches('0');
        format!("{sign}{whole}.{trimmed}")
    }

    /// Whole units when this value has no fractional millionths.
    pub fn whole_units(self) -> Option<i64> {
        if self.0 % Self::SCALE == 0 {
            Some(self.0 / Self::SCALE)
        } else {
            None
        }
    }

    /// `None` when the sum does not fit in `i64` micros, instead of panicking.
    /// Use this to add amounts from untrusted input, such as CSV rows.
    ///
    /// `Micros` deliberately has no `Add`, `AddAssign` or `Sum` impl: every sum
    /// must choose between this checked form and an explicit saturating one,
    /// so no `+` on money can panic on overflow.
    pub fn checked_add(self, rhs: Self) -> Option<Self> {
        self.0.checked_add(rhs.0).map(Self)
    }
}

impl std::str::FromStr for Micros {
    type Err = DecimalError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::parse(raw)
    }
}

impl std::fmt::Display for Micros {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_decimal_string())
    }
}

/// `power` is the base-10 exponent applied to `significand` to produce micros.
fn scale_pow10(mut significand: i128, power: i32) -> Result<i128, DecimalError> {
    if significand == 0 {
        return Ok(0);
    }
    if power >= 0 {
        for _ in 0..power {
            significand = significand
                .checked_mul(10)
                .ok_or(DecimalError::NotANumber)?;
        }
        return Ok(significand);
    }
    // More than ~38 trailing zeros cannot divide a non-zero i128 down to an integer micro.
    if power < -40 {
        return Err(DecimalError::TooManyDecimals);
    }
    for _ in 0..(-power) {
        if significand % 10 != 0 {
            return Err(DecimalError::TooManyDecimals);
        }
        significand /= 10;
    }
    Ok(significand)
}

fn split_exponent(unsigned: &str) -> Result<(&str, i32), DecimalError> {
    let Some(index) = unsigned
        .bytes()
        .position(|byte| byte == b'e' || byte == b'E')
    else {
        return Ok((unsigned, 0));
    };
    let (coefficient, rest) = unsigned.split_at(index);
    let exp_text = &rest[1..];
    if coefficient.is_empty() || exp_text.is_empty() {
        return Err(DecimalError::NotANumber);
    }
    let (exp_neg, digits) = match exp_text.as_bytes()[0] {
        b'+' => (false, &exp_text[1..]),
        b'-' => (true, &exp_text[1..]),
        _ => (false, exp_text),
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(DecimalError::NotANumber);
    }
    let mut exp: i32 = 0;
    for byte in digits.bytes() {
        exp = exp
            .checked_mul(10)
            .and_then(|value| value.checked_add(i32::from(byte - b'0')))
            .ok_or(DecimalError::NotANumber)?;
    }
    Ok((coefficient, if exp_neg { -exp } else { exp }))
}

/// Integer significand and how many of its digits sit after the decimal point.
fn parse_coefficient(coefficient: &str) -> Result<(i128, i32), DecimalError> {
    let (whole, frac) = match coefficient.split_once('.') {
        Some((whole, frac)) => {
            if frac.contains('.') {
                return Err(DecimalError::NotANumber);
            }
            (whole, frac)
        }
        None => (coefficient, ""),
    };
    if whole.is_empty() && frac.is_empty() {
        return Err(DecimalError::NotANumber);
    }
    if !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !frac.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(DecimalError::NotANumber);
    }
    let mut significand: i128 = 0;
    for byte in whole.bytes().chain(frac.bytes()) {
        significand = significand
            .checked_mul(10)
            .and_then(|value| value.checked_add(i128::from(byte - b'0')))
            .ok_or(DecimalError::NotANumber)?;
    }
    let frac_len = i32::try_from(frac.len()).map_err(|_| DecimalError::NotANumber)?;
    Ok((significand, frac_len))
}

/// `quantity * unit_price` in micros, rounded half away from zero.
///
/// Both factors are already in micros, so the raw product is in 10^-12 dollars.
/// Dividing by [`Micros::SCALE`] returns micros. A remainder of half a micro
/// rounds away from zero, the same rule as cent display.
pub(crate) fn rounded_product(quantity: Micros, unit_price: Micros) -> i128 {
    let product = i128::from(quantity.0) * i128::from(unit_price.0);
    div_half_away(product, i128::from(Micros::SCALE))
}

/// Six digits after the decimal point, same shape as [`Micros::to_decimal_string`].
pub(crate) fn format_micros(value: i128) -> String {
    format_fixed(value, 6)
}

fn format_fixed(value: i128, digits: usize) -> String {
    let neg = value < 0;
    let abs = value.unsigned_abs();
    let scale = 10u128.pow(u32::try_from(digits).expect("decimal width fits u32"));
    let whole = abs / scale;
    let frac = abs % scale;
    let sign = if neg && abs != 0 { "-" } else { "" };
    format!("{sign}{whole}.{frac:0digits$}")
}

/// Percentage `net / total * 100`, half-up, with exactly `digits` decimal places.
/// A zero total is `0` with that many decimals.
pub fn percent_fixed(net: Micros, total: Micros, digits: u32) -> String {
    if total.0 == 0 {
        return format_fixed(0, digits as usize);
    }
    let scale = 10i128.pow(digits);
    let num = i128::from(net.0) * 100 * scale;
    let den = i128::from(total.0);
    format_fixed(div_half_away(num, den), digits as usize)
}

pub(crate) fn div_half_away(num: i128, den: i128) -> i128 {
    let quot = num / den;
    let rem = num % den;
    if rem != 0 && rem.abs() * 2 >= den.abs() {
        quot + num.signum() * den.signum()
    } else {
        quot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usd(text: &str) -> Micros {
        Micros::parse(text).unwrap()
    }

    #[test]
    fn parses_plain_decimals_without_rounding() {
        assert_eq!(usd("0").0, 0);
        assert_eq!(usd("0.00").0, 0);
        assert_eq!(usd("1").0, 1_000_000);
        assert_eq!(usd("1.").0, 1_000_000);
        assert_eq!(usd(".5").0, 500_000);
        assert_eq!(usd("+1.5").0, 1_500_000);
        assert_eq!(usd("-1.5").0, -1_500_000);
        assert_eq!(usd("-.5").0, -500_000);
        assert_eq!(usd("00.50").0, 500_000);
        assert_eq!(usd("7.316").0, 7_316_000);
        assert_eq!(usd("0.000336").0, 336);
        assert_eq!(usd("0.000001").0, 1);
        assert_eq!(usd("1.234567").0, 1_234_567);
        assert_eq!(usd("44.232").to_decimal_string(), "44.232000");
        assert_eq!(usd("-0.25").to_decimal_string(), "-0.250000");
    }

    #[test]
    fn exponent_form_stays_exact() {
        assert_eq!(usd("1e1").0, 10_000_000);
        assert_eq!(usd("10e0").0, 10_000_000);
        assert_eq!(usd("0e0").0, 0);
        assert_eq!(usd("1E-1").0, 100_000);
        assert_eq!(usd("1.25e-1").0, 125_000);
        assert_eq!(usd("10e-7").0, 1);
        assert_eq!(usd("1.2300000").0, 1_230_000);
        assert_eq!(usd("1.2345670").0, 1_234_567);
        assert_eq!(usd("10000000e-7").0, 1_000_000);
    }

    #[test]
    fn rejects_non_numbers_and_a_seventh_decimal() {
        for raw in [
            "",
            " ",
            "+",
            "-",
            ".",
            "e10",
            "1e",
            "1e+",
            "1e-",
            "1e2e3",
            "1.2.3",
            "NaN",
            "nan",
            "+NaN",
            "inf",
            "INF",
            "+inf",
            "-inf",
            "Infinity",
            "-Infinity",
            "abc",
            "1,000",
            "10usd",
            "1e309",
            "--",
        ] {
            assert_eq!(Micros::parse(raw), Err(DecimalError::NotANumber), "{raw}");
        }
        for raw in [
            "1.2345678",
            "0.0000001",
            "1e-7",
            "12345678e-7",
            "-0.0000001",
        ] {
            assert_eq!(
                Micros::parse(raw),
                Err(DecimalError::TooManyDecimals),
                "{raw}"
            );
        }
    }

    #[test]
    fn cents_round_half_up_away_from_zero() {
        assert_eq!(usd("0").format_cents(), "0.00");
        assert_eq!(usd("0.005").format_cents(), "0.01");
        assert_eq!(usd("2.112").format_cents(), "2.11");
        assert_eq!(usd("2.115").format_cents(), "2.12");
        assert_eq!(usd("1.004").format_cents(), "1.00");
        assert_eq!(usd("1.005").format_cents(), "1.01");
        assert_eq!(usd("-0.005").format_cents(), "-0.01");
        assert_eq!(usd("-0.004").format_cents(), "0.00");
        assert_eq!(usd("284.208").format_cents(), "284.21");
        assert_eq!(usd("44.232").format_cents(), "44.23");
        assert_eq!(usd("230.144").format_cents(), "230.14");
    }

    #[test]
    fn quantity_trims_to_three_decimals() {
        assert_eq!(usd("3712").format_quantity(), "3712");
        assert_eq!(usd("0").format_quantity(), "0");
        assert_eq!(usd("0.5").format_quantity(), "0.5");
        assert_eq!(usd("10.125").format_quantity(), "10.125");
        assert_eq!(usd("1.2345").format_quantity(), "1.235");
        assert_eq!(usd("-1.2").format_quantity(), "-1.2");
    }

    #[test]
    fn product_118_minutes_at_0_062_is_7_316() {
        assert_eq!(rounded_product(usd("118"), usd("0.062")), 7_316_000);
        assert_eq!(
            format_micros(rounded_product(usd("118"), usd("0.062"))),
            "7.316000"
        );
    }

    #[test]
    fn product_rounds_half_away_from_zero_before_the_micro() {
        // 0.5 * 0.000336 = 0.000168 exactly (storage sample row).
        assert_eq!(rounded_product(usd("0.5"), usd("0.000336")), 168);
        // 0.5 * 0.000001 = 0.5 micros -> 1 micro.
        assert_eq!(rounded_product(usd("0.5"), usd("0.000001")), 1);
        assert_eq!(rounded_product(usd("-0.5"), usd("0.000001")), -1);
        // 0.1 micro rounds down to 0.
        assert_eq!(rounded_product(usd("0.1"), usd("0.000001")), 0);
        assert_eq!(rounded_product(usd("0"), usd("9.99")), 0);
        assert_eq!(rounded_product(usd("118"), usd("0.080")), 9_440_000);
        assert_eq!(format_micros(-1), "-0.000001");
    }

    #[test]
    fn percent_half_up_matches_known_ratios() {
        assert_eq!(percent_fixed(usd("3"), usd("4"), 2), "75.00");
        assert_eq!(percent_fixed(usd("1"), usd("4"), 2), "25.00");
        assert_eq!(percent_fixed(usd("1"), usd("3"), 2), "33.33");
        assert_eq!(percent_fixed(usd("1"), usd("3"), 6), "33.333333");
        assert_eq!(percent_fixed(usd("2"), usd("3"), 2), "66.67");
        assert_eq!(percent_fixed(usd("2"), usd("3"), 6), "66.666667");
        assert_eq!(percent_fixed(usd("1"), Micros::ZERO, 2), "0.00");
        assert_eq!(percent_fixed(usd("230.144"), usd("284.208"), 2), "80.98");
    }
    #[test]
    fn checked_add_reports_overflow_instead_of_panicking() {
        assert_eq!(usd("1.25").checked_add(usd("2.5")), Some(usd("3.75")));
        assert_eq!(usd("-1").checked_add(usd("1")), Some(Micros::ZERO));
        assert_eq!(Micros(i64::MAX).checked_add(Micros(1)), None);
        assert_eq!(Micros(i64::MIN).checked_add(Micros(-1)), None);
        assert_eq!(
            Micros(i64::MAX).checked_add(Micros(i64::MIN)),
            Some(Micros(-1))
        );
    }
}
