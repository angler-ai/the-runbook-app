//! Exact decimal operations bounded by spelling size, never exponent magnitude.
// A JSON exponent has no fixed size. Add a small decimal-point adjustment
// directly to its decimal digits instead of overflowing machine integers or
// allocating a coefficient with a potentially enormous number of zeroes.
pub(crate) fn shifted_exponent(text: &str, shift: i128) -> (bool, String) {
    if let Some(value) = text
        .parse::<i128>()
        .ok()
        .and_then(|value| value.checked_add(shift))
    {
        return (value < 0, value.unsigned_abs().to_string());
    }
    let negative = text.starts_with('-');
    let digits = text.trim_start_matches(['-', '+']).trim_start_matches('0');
    let mut carry = if negative { -shift } else { shift };
    let mut result = Vec::with_capacity(digits.len() + 1);
    for digit in digits.bytes().rev() {
        let value = i128::from(digit - b'0') + carry;
        result.push(b'0' + value.rem_euclid(10) as u8);
        carry = value.div_euclid(10);
    }
    while carry > 0 {
        result.push(b'0' + (carry % 10) as u8);
        carry /= 10;
    }
    result.reverse();
    let result = String::from_utf8(result).expect("decimal digits");
    (negative, result.trim_start_matches('0').to_owned())
}

/// Canonical exact number text. Integer spellings stay integers. Decimal forms
/// choose the shorter of plain and normalized scientific notation (plain on a
/// tie), always with a fractional digit. No exponent-sized allocation or f64.
pub(crate) fn canonical(text: &str) -> String {
    let negative = text.starts_with('-');
    let unsigned = text.trim_start_matches('-');
    if !unsigned.contains(['.', 'e', 'E']) {
        return if unsigned.bytes().all(|b| b == b'0') {
            "0".into()
        } else {
            text.into()
        };
    }
    let (mantissa, exponent) = unsigned.split_once(['e', 'E']).unwrap_or((unsigned, "0"));
    let fraction = mantissa.split_once('.').map_or(0, |(_, f)| f.len());
    let digits: String = mantissa
        .bytes()
        .filter(|b| *b != b'.')
        .map(char::from)
        .collect();
    let significant = digits.trim_start_matches('0');
    if significant.is_empty() {
        return "0.0".into();
    }
    let (exp_negative, exp_digits) =
        shifted_exponent(exponent, significant.len() as i128 - fraction as i128 - 1);
    let significant = significant.trim_end_matches('0');
    let sign = if negative { "-" } else { "" };
    let mut scientific = format!(
        "{sign}{}.{}",
        &significant[..1],
        if significant.len() == 1 {
            "0"
        } else {
            &significant[1..]
        }
    );
    if exp_digits != "0" {
        scientific.push('e');
        if exp_negative {
            scientific.push('-');
        }
        scientific.push_str(&exp_digits);
    }
    // Only construct a plain spelling if its length is no larger than the
    // already bounded scientific spelling. Huge signed exponents stay compact.
    let Ok(magnitude) = exp_digits.parse::<usize>() else {
        return scientific;
    };
    let plain_length = if exp_negative {
        magnitude
            .checked_add(1)
            .and_then(|n| n.checked_add(significant.len()))
    } else {
        magnitude.checked_add(1).and_then(|split| {
            if split >= significant.len() {
                split.checked_add(2)
            } else {
                significant.len().checked_add(1)
            }
        })
    }
    .and_then(|n| n.checked_add(sign.len()));
    if !plain_length.is_some_and(|n| n <= scientific.len()) {
        return scientific;
    }
    let mut plain = String::with_capacity(plain_length.unwrap());
    plain.push_str(sign);
    if exp_negative {
        plain.push_str("0.");
        plain.extend(std::iter::repeat_n('0', magnitude - 1));
        plain.push_str(significant);
    } else {
        let split = magnitude + 1;
        if split >= significant.len() {
            plain.push_str(significant);
            plain.extend(std::iter::repeat_n('0', split - significant.len()));
            plain.push_str(".0");
        } else {
            plain.push_str(&significant[..split]);
            plain.push('.');
            plain.push_str(&significant[split..]);
        }
    }
    plain
}
