//! Bounded arithmetic parser. User input is never executed as code.
/// All-mode calculation needs an operation, a function call, a conversion or an
/// explicit '='. Numeric names and date/version searches remain normal searches.
pub fn automatic(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() || text.len() > 512 {
        return None;
    }
    if text.starts_with('=') {
        return calculate(text);
    }
    if date_like(text) {
        return None;
    }
    let function = [
        "sqrt", "abs", "sin", "cos", "tan", "ln", "log", "log10", "round", "floor", "ceil",
    ]
    .iter()
    .any(|name| {
        text.get(..name.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(name))
            && text[name.len()..].trim_start().starts_with('(')
    });
    let operation = text.char_indices().any(|(at, c)| match c {
        '+' | '-' => {
            let left = text[..at].trim_end();
            let exponent = left.ends_with(['e', 'E'])
                && left
                    .as_bytes()
                    .iter()
                    .rev()
                    .nth(1)
                    .is_some_and(|b| b.is_ascii_digit() || *b == b'.');
            !exponent
                && left.chars().last().is_some_and(|last| {
                    last.is_ascii_alphanumeric() || matches!(last, ')' | '%' | 'π')
                })
        }
        '*' | '×' | '/' | '÷' | '^' | '%' => true,
        _ => false,
    });
    if function || operation || convert(text).is_some() {
        calculate(text)
    } else {
        None
    }
}

fn date_like(text: &str) -> bool {
    let parts: Vec<_> = text.split(['-', '/']).collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        && ((parts[0].len() == 4 && parts[1].len() <= 2 && parts[2].len() <= 2)
            || (parts[0].len() <= 2 && parts[1].len() <= 2 && parts[2].len() == 4))
}

pub fn calculate(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() || text.len() > 512 {
        return None;
    }
    let text = text.strip_prefix('=').unwrap_or(text).trim();
    let text = text.strip_suffix('=').unwrap_or(text).trim();
    if let Some(value) = convert(text) {
        return Some(value);
    }
    let mut parser = Parser {
        text,
        at: 0,
        depth: 0,
    };
    let value = parser.expression(0)?;
    parser.space();
    if parser.at != text.len() || !value.is_finite() {
        return None;
    }
    Some(format_number(value))
}
fn format_number(value: f64) -> String {
    let value = if value == 0.0 { 0.0 } else { value };
    if value.abs() >= 1e15 || value != 0.0 && value.abs() < 1e-9 {
        return format!("{value:.10e}");
    }
    let result = format!("{value:.10}");
    result
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}
struct Parser<'a> {
    text: &'a str,
    at: usize,
    depth: u8,
}
impl Parser<'_> {
    fn space(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.take();
        }
    }
    fn peek(&self) -> Option<char> {
        self.text[self.at..].chars().next()
    }
    fn take(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.at += c.len_utf8();
        Some(c)
    }
    fn expression(&mut self, min: u8) -> Option<f64> {
        self.depth += 1;
        if self.depth > 32 {
            return None;
        }
        self.space();
        let mut left = match self.peek()? {
            '+' | '-' => {
                let sign = self.take()?;
                let value = self.expression(3)?;
                if sign == '-' { -value } else { value }
            }
            '(' => {
                self.take();
                let value = self.expression(0)?;
                self.space();
                if self.take()? != ')' {
                    return None;
                }
                value
            }
            c if c.is_ascii_digit() || c == '.' => self.number()?,
            c if c.is_ascii_alphabetic() || c == 'π' => self.identifier()?,
            _ => return None,
        };
        loop {
            self.space();
            if self.peek() == Some('%') {
                self.take();
                left /= 100.0;
                continue;
            }
            let Some(op) = self.peek() else {
                break;
            };
            let precedence = match op {
                '+' | '-' => 1,
                '*' | '×' | '/' | '÷' => 2,
                '^' => 4,
                _ => break,
            };
            if precedence < min {
                break;
            }
            self.take();
            let right = self.expression(if op == '^' {
                precedence
            } else {
                precedence + 1
            })?;
            left = match op {
                '+' => left + right,
                '-' => left - right,
                '*' | '×' => left * right,
                '/' | '÷' => left / right,
                '^' => left.powf(right),
                _ => return None,
            };
            if !left.is_finite() {
                return None;
            }
        }
        self.depth -= 1;
        Some(left)
    }
    fn number(&mut self) -> Option<f64> {
        let start = self.at;
        while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '.') {
            self.take();
        }
        if self.peek().is_some_and(|c| c == 'e' || c == 'E') {
            self.take();
            if self.peek().is_some_and(|c| c == '+' || c == '-') {
                self.take();
            }
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.take();
            }
        }
        self.text[start..self.at].parse().ok()
    }
    fn identifier(&mut self) -> Option<f64> {
        let start = self.at;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == 'π')
        {
            self.take();
        }
        let name = &self.text[start..self.at];
        match name.to_ascii_lowercase().as_str() {
            "pi" | "π" => Some(std::f64::consts::PI),
            "e" => Some(std::f64::consts::E),
            "tau" => Some(std::f64::consts::TAU),
            name => {
                self.space();
                if self.take()? != '(' {
                    return None;
                }
                let value = self.expression(0)?;
                self.space();
                if self.take()? != ')' {
                    return None;
                }
                let result = match name {
                    "sqrt" => value.sqrt(),
                    "abs" => value.abs(),
                    "sin" => value.sin(),
                    "cos" => value.cos(),
                    "tan" => value.tan(),
                    "ln" => value.ln(),
                    "log" | "log10" => value.log10(),
                    "round" => value.round(),
                    "floor" => value.floor(),
                    "ceil" => value.ceil(),
                    _ => return None,
                };
                result.is_finite().then_some(result)
            }
        }
    }
}
fn convert(text: &str) -> Option<String> {
    let tokens: Vec<_> = text.split_whitespace().collect();
    if tokens.len() != 4
        || !(tokens[2].eq_ignore_ascii_case("to") || tokens[2].eq_ignore_ascii_case("in"))
    {
        return None;
    }
    let value: f64 = tokens[0].parse().ok()?;
    let from = tokens[1].to_ascii_lowercase();
    let to = tokens[3].to_ascii_lowercase();
    let unit = |name: &str| -> Option<(u8, f64, f64)> {
        Some(match name {
            "mm" => (0, 0.001, 0.0),
            "cm" => (0, 0.01, 0.0),
            "m" | "meters" => (0, 1.0, 0.0),
            "km" => (0, 1000.0, 0.0),
            "in" | "inches" => (0, 0.0254, 0.0),
            "ft" | "feet" => (0, 0.3048, 0.0),
            "yd" => (0, 0.9144, 0.0),
            "mi" | "miles" => (0, 1609.344, 0.0),
            "mg" => (1, 0.000001, 0.0),
            "g" => (1, 0.001, 0.0),
            "kg" => (1, 1.0, 0.0),
            "lb" | "lbs" => (1, 0.45359237, 0.0),
            "oz" => (1, 0.028349523125, 0.0),
            "ms" => (2, 0.001, 0.0),
            "s" | "sec" => (2, 1.0, 0.0),
            "min" => (2, 60.0, 0.0),
            "h" | "hours" => (2, 3600.0, 0.0),
            "days" => (2, 86400.0, 0.0),
            "c" | "°c" => (3, 1.0, 273.15),
            "f" | "°f" => (3, 5.0 / 9.0, 255.3722222222222),
            "k" => (3, 1.0, 0.0),
            "b" => (4, 1.0, 0.0),
            "kb" => (4, 1000.0, 0.0),
            "mb" => (4, 1e6, 0.0),
            "gb" => (4, 1e9, 0.0),
            "kib" => (4, 1024.0, 0.0),
            "mib" => (4, 1048576.0, 0.0),
            "gib" => (4, 1073741824.0, 0.0),
            _ => return None,
        })
    };
    let (a, scale_a, offset_a) = unit(&from)?;
    let (b, scale_b, offset_b) = unit(&to)?;
    if a != b {
        return None;
    }
    let result = (value * scale_a + offset_a - offset_b) / scale_b;
    result
        .is_finite()
        .then(|| format!("{} {}", format_number(result), tokens[3]))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arithmetic_is_finite_bounded_and_obeys_precedence() {
        for (q, result) in [
            ("2+3*4", "14"),
            ("-2^2", "-4"),
            ("2^3^2", "512"),
            ("sqrt(81)", "9"),
            ("50% * 20", "10"),
            ("32 f to c", "0 c"),
            ("1 gib to mib", "1024 mib"),
        ] {
            assert_eq!(calculate(q).as_deref(), Some(result));
        }
        for q in [
            "1/0",
            "sqrt(-1)",
            "2+",
            "exit()",
            "999^999",
            &"(".repeat(200),
        ] {
            assert!(calculate(q).is_none());
        }
    }
    #[test]
    fn all_mode_detects_math_without_taking_over_filename_searches() {
        for (query, answer) in [
            ("2+3*4", "14"),
            ("7-2", "5"),
            ("6/2", "3"),
            ("sqrt(81)", "9"),
            ("ABS (-5)", "5"),
            ("pi-pi", "0"),
            ("e-e", "0"),
            ("=123", "123"),
            ("2 + 2 =", "4"),
            ("1 gib TO mib", "1024 mib"),
        ] {
            assert_eq!(automatic(query).as_deref(), Some(answer), "{query}");
        }
        for query in [
            "123",
            "2026",
            "-42",
            "1.2",
            "1e3",
            "1e-3",
            "1e+3",
            "pi",
            "e",
            "tau",
            "2026-10-23",
            "23/10/2026",
            "2026/10/23",
            "report-2026.pdf",
            "123.txt",
            "1.2.3",
            "C:/2/3",
            "sqrt_notes",
            "log10",
            "2+",
            "1/0",
            "sqrt(-1)",
            "==123",
            "hello + world",
        ] {
            assert!(automatic(query).is_none(), "{query}");
        }
    }
}
