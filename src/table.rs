//! CSV files, turned into sentences that make sense when heard. A table read out
//! cell by cell is hard to follow by ear, as nobody can see which column a value
//! is in, so:
//!
//! * The reading starts by saying how many rows there are and what the columns
//!   are, and anything every row shares, such as "All amounts are in pounds."
//! * Each row is one sentence that names each value's column, led by the row's
//!   own name where the first column gives one:
//!   `Transaction ID TXN-0001: date 1 January 2026, description Salary and amount 2,850 pounds.`
//! * Dates such as 2026-01-31 are said as 31 January 2026, and amounts of
//!   money, from a currency symbol or a currency column, as pounds and pence
//!   (or dollars, euros and cents), with "minus" for a negative amount.
//! * Empty cells are left out, rather than read as nothing.
//!
//! Files are untrusted, but this only rearranges their text: there is nothing
//! to expand, and `document::read_text` has already limited the file's size.

use anyhow::bail;

/// Months, for saying dates.
const MONTHS: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// Turns a CSV file's text into sentences to read aloud.
pub fn speakable(text: &str) -> anyhow::Result<String> {
    let mut rows = parse(text);
    if rows.is_empty() {
        bail!("no text was found in this file");
    }
    let header = has_header(&rows).then(|| rows.remove(0));
    let width = rows.iter().chain(&header).map(Vec::len).max().unwrap_or(0);
    let names: Vec<String> = (0..width)
        .map(|c| match header.as_ref().and_then(|h| h.get(c)).map(|h| column_name(h)) {
            Some(name) if !name.is_empty() => name,
            _ => format!("column {}", c + 1),
        })
        .collect();
    let columns: Vec<Column> = (0..width).map(|c| Column::of(c, &rows, header.as_ref())).collect();
    let currency = currency_column(&columns, &rows);

    let mut out = introduction(&rows, header.is_some(), &names, &columns, currency);
    let label_column = label_column(&rows, &columns, header.is_some());
    for (number, row) in rows.iter().enumerate() {
        out.push('\n');
        out.push_str(&row_sentence(number, row, &names, &columns, currency, label_column));
    }
    Ok(out)
}

/// The cells of each line, as RFC 4180 has them: values in double quotes can
/// hold the separator, line breaks and doubled quotes. The separator is a
/// comma, unless the first line has more semicolons or tabs, as files from
/// spreadsheets in much of Europe do. Lines with nothing in them are dropped.
fn parse(text: &str) -> Vec<Vec<String>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let separator = separator(text);
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    cell.push('"');
                }
                '"' => quoted = false,
                _ => cell.push(c),
            }
        } else if c == '"' && cell.trim().is_empty() {
            cell.clear();
            quoted = true;
        } else if c == separator {
            row.push(std::mem::take(&mut cell));
        } else if c == '\n' || c == '\r' {
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            row.push(std::mem::take(&mut cell));
            rows.push(std::mem::take(&mut row));
        } else {
            cell.push(c);
        }
    }
    row.push(cell);
    rows.push(row);
    rows.into_iter()
        .map(|row| row.into_iter().map(|cell| cell.split_whitespace().collect::<Vec<_>>().join(" ")).collect::<Vec<_>>())
        .filter(|row| row.iter().any(|cell| !cell.is_empty()))
        .collect()
}

fn separator(text: &str) -> char {
    let first = text.lines().next().unwrap_or("");
    let count = |c| first.chars().filter(|x| *x == c).count();
    [',', ';', '\t'].into_iter().max_by_key(|c| (count(*c), *c == ',')).unwrap_or(',')
}

/// Whether the first line names the columns: each of its cells has words in,
/// none is a number or a date, and no two are the same.
fn has_header(rows: &[Vec<String>]) -> bool {
    let Some(first) = rows.first() else { return false };
    if rows.len() < 2 {
        return false;
    }
    let mut seen = std::collections::HashSet::new();
    first.iter().all(|cell| {
        cell.chars().any(char::is_alphabetic)
            && amount(cell).is_none()
            && date(cell).is_none()
            && seen.insert(cell.to_lowercase())
    })
}

/// A column's name as it is said in the middle of a sentence: "Date" becomes
/// "date" and "unit_price" becomes "unit price", but "ID" stays as it is.
fn column_name(header: &str) -> String {
    let words = header.replace('_', " ");
    let words = words.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = words.chars();
    match (chars.next(), chars.next()) {
        (Some(first), Some(second)) if first.is_uppercase() && second.is_lowercase() => {
            first.to_lowercase().chain(words.chars().skip(1)).collect()
        }
        _ => words,
    }
}

/// An amount as written: the sign, the whole part's digits, any decimal
/// digits, and any currency symbol.
#[derive(Debug, PartialEq)]
struct Amount {
    negative: bool,
    whole: String,
    fraction: Option<String>,
    symbol: Option<char>,
}

/// Reads numbers such as `-950.00`, `£1,234.50`, `-£5`, `£-5` or `(42.10)`,
/// the brackets being how accounts show a negative amount.
fn amount(text: &str) -> Option<Amount> {
    let mut s = text.trim();
    let mut negative = false;
    if let Some(inner) = s.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
        negative = true;
        s = inner.trim();
    }
    let mut symbol = None;
    let mut take_sign = |s: &mut &str| {
        if let Some(rest) = s.strip_prefix('-').or_else(|| s.strip_prefix('\u{2212}')) {
            negative = !negative;
            *s = rest;
        } else if let Some(rest) = s.strip_prefix('+') {
            *s = rest;
        }
    };
    take_sign(&mut s);
    for c in ['£', '$', '€'] {
        if let Some(rest) = s.strip_prefix(c) {
            symbol = Some(c);
            s = rest;
        }
    }
    take_sign(&mut s);
    let (whole, fraction) = match s.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (s, None),
    };
    // Commas only as thousands separators, so "1,2" isn't taken for a number.
    let groups: Vec<&str> = whole.split(',').collect();
    let grouped_well =
        groups.len() == 1 || (!groups[0].is_empty() && groups[0].len() <= 3 && groups[1..].iter().all(|g| g.len() == 3));
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    if !grouped_well || !groups.iter().all(|g| digits(g)) || fraction.is_some_and(|f| !digits(f)) {
        return None;
    }
    Some(Amount { negative, whole: groups.concat(), fraction: fraction.map(str::to_owned), symbol })
}

/// Reads an ISO 8601 date, `2026-01-31`, perhaps with a time after it, as
/// (year, month, day, time).
fn date(text: &str) -> Option<(u32, usize, u32, Option<String>)> {
    let text = text.trim();
    let (day_part, time) = match text.split_once(['T', ' ']) {
        Some((d, t)) => (d, Some(t)),
        None => (text, None),
    };
    let mut parts = day_part.split('-');
    let (year, month, day) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return None;
    }
    let (year, month, day): (u32, usize, u32) = (year.parse().ok()?, month.parse().ok()?, day.parse().ok()?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let time = match time {
        None => None,
        Some(t) => {
            // Hours and minutes are what anyone wants to hear.
            let t = t.trim_end_matches('Z');
            let hm = t.get(..5).filter(|hm| {
                let b = hm.as_bytes();
                b[2] == b':' && b[..2].iter().chain(&b[3..]).all(u8::is_ascii_digit)
            })?;
            Some(hm.to_owned())
        }
    };
    Some((year, month, day, time))
}

fn say_date((year, month, day, time): (u32, usize, u32, Option<String>)) -> String {
    let mut out = format!("{day} {} {year}", MONTHS[month - 1]);
    if let Some(time) = time {
        out.push_str(" at ");
        out.push_str(&time);
    }
    out
}

/// Digits with commas between the thousands, which voices read as one number.
fn thousands(digits: &str) -> String {
    let digits = digits.trim_start_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A currency's spoken names: one and many of the main unit, then of the
/// hundredth.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Currency {
    one: &'static str,
    many: &'static str,
    one_hundredth: &'static str,
    hundredths: &'static str,
    /// What "All amounts are in ..." says.
    name: &'static str,
}

const POUNDS: Currency = Currency { one: "pound", many: "pounds", one_hundredth: "penny", hundredths: "pence", name: "pounds" };
const DOLLARS: Currency = Currency { one: "dollar", many: "dollars", one_hundredth: "cent", hundredths: "cents", name: "dollars" };
const EUROS: Currency = Currency { one: "euro", many: "euros", one_hundredth: "cent", hundredths: "cents", name: "euros" };

fn currency_of_symbol(symbol: char) -> Option<Currency> {
    match symbol {
        '£' => Some(POUNDS),
        '$' => Some(DOLLARS),
        '€' => Some(EUROS),
        _ => None,
    }
}

fn currency_of_code(code: &str) -> Option<Currency> {
    match code.trim().to_ascii_uppercase().as_str() {
        "GBP" => Some(POUNDS),
        "USD" => Some(DOLLARS),
        "EUR" => Some(EUROS),
        _ => None,
    }
}

/// How an amount of money is said, such as "minus 68 pounds 24 pence". In a
/// currency without spoken names here, it is the number and the code.
fn say_money(a: &Amount, currency: Option<Currency>, code: &str) -> String {
    let sign = if a.negative { "minus " } else { "" };
    let Some(c) = currency else {
        let fraction = a.fraction.as_ref().map_or(String::new(), |f| format!(".{f}"));
        let code = if code.is_empty() { String::new() } else { format!(" {}", code.trim()) };
        return format!("{sign}{}{fraction}{code}", thousands(&a.whole));
    };
    // Hundredths, from however many decimal places there are.
    let hundredths: u32 = a.fraction.as_ref().map_or(0, |f| format!("{f:0<2}")[..2].parse().unwrap_or(0));
    let whole = thousands(&a.whole);
    let units = |n: &str, one: &'static str, many: &'static str| format!("{n} {}", if n == "1" { one } else { many });
    let main = units(&whole, c.one, c.many);
    let small = units(&hundredths.to_string(), c.one_hundredth, c.hundredths);
    match (whole.as_str(), hundredths) {
        (_, 0) => format!("{sign}{main}"),
        ("0", _) => format!("{sign}{small}"),
        _ => format!("{sign}{main} {small}"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    /// Amounts of money: with a currency symbol, or with two decimal places
    /// beside a currency column.
    Money,
    Date,
    Number,
    Text,
}

#[derive(Debug)]
struct Column {
    kind: Kind,
    /// The value every row has, if every row has the same one.
    same_everywhere: Option<String>,
    /// The column says which currency the amounts are in.
    is_currency: bool,
    /// Every row has a value, and no two are the same.
    unique: bool,
}

impl Column {
    fn of(c: usize, rows: &[Vec<String>], header: Option<&Vec<String>>) -> Self {
        let values: Vec<&str> = rows.iter().filter_map(|r| r.get(c)).map(String::as_str).filter(|v| !v.is_empty()).collect();
        let all = |test: &dyn Fn(&str) -> bool| !values.is_empty() && values.iter().all(|v| test(v));
        let amounts: Option<Vec<Amount>> = values.iter().map(|v| amount(v)).collect();
        let kind = match &amounts {
            Some(a) if !a.is_empty() && a.iter().all(|a| a.fraction.as_ref().is_some_and(|f| f.len() == 2)) => Kind::Money,
            Some(a) if a.iter().any(|a| a.symbol.is_some()) => Kind::Money,
            Some(a) if !a.is_empty() => Kind::Number,
            _ if all(&|v| date(v).is_some()) => Kind::Date,
            _ => Kind::Text,
        };
        // Only a column with a name can be said once for the whole table.
        let same_everywhere =
            (header.is_some() && rows.len() >= 3 && values.len() == rows.len() && values.iter().all(|v| *v == values[0]))
                .then(|| values[0].to_owned());
        let named_currency = header.and_then(|h| h.get(c)).is_some_and(|h| h.to_lowercase().contains("currency"));
        let is_currency = kind == Kind::Text && (named_currency || all(&|v| currency_of_code(v).is_some()));
        let mut seen = std::collections::HashSet::new();
        let unique = values.len() == rows.len() && values.iter().all(|v| seen.insert(*v));
        Self { kind, same_everywhere, is_currency, unique }
    }
}

/// The column saying which currency each row's amounts are in, if there is one.
fn currency_column(columns: &[Column], rows: &[Vec<String>]) -> Option<usize> {
    let column = columns.iter().position(|c| c.is_currency)?;
    // Two decimal places are only taken for money when a column says so.
    let has_money = columns.iter().any(|c| c.kind == Kind::Money);
    (has_money && !rows.is_empty()).then_some(column)
}

/// Whether a value that looks like money really is, in this table.
fn is_money(column: &Column, value: &Amount, currency: Option<usize>) -> bool {
    column.kind == Kind::Money && (value.symbol.is_some() || currency.is_some())
}

/// The column that names each row, if the first does: a column of text with a
/// different value in every row, such as an ID or a name.
fn label_column(rows: &[Vec<String>], columns: &[Column], header: bool) -> Option<usize> {
    let first = columns.first()?;
    (header && rows.len() > 1 && first.unique && first.kind == Kind::Text && !first.is_currency).then_some(0)
}

/// "a, b and c".
fn list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn introduction(
    rows: &[Vec<String>],
    header: bool,
    names: &[String],
    columns: &[Column],
    currency: Option<usize>,
) -> String {
    let rows_text = plural(rows.len(), "row", "rows");
    let mut out = if header {
        format!("This table has {rows_text}, with the columns {}.", list(names))
    } else {
        format!("This table has {rows_text}, each with {}.", plural(names.len(), "column", "columns"))
    };
    let code = currency.and_then(|c| rows.first()?.get(c)).map_or("", String::as_str);
    for (c, column) in columns.iter().enumerate() {
        let Some(value) = &column.same_everywhere else { continue };
        if Some(c) == currency {
            match currency_of_code(value) {
                Some(found) => out.push_str(&format!(" All amounts are in {}.", found.name)),
                None => out.push_str(&format!(" All amounts are in {value}.")),
            }
        } else {
            out.push_str(&format!(" In every row, the {} is {}.", names[c], say(column, value, currency, code)));
        }
    }
    out
}

/// A cell's value as it is said.
fn say(column: &Column, value: &str, currency: Option<usize>, code: &str) -> String {
    match column.kind {
        Kind::Date => date(value).map_or_else(|| value.to_owned(), say_date),
        Kind::Money | Kind::Number => match amount(value) {
            Some(a) if is_money(column, &a, currency) => {
                let known = a.symbol.and_then(currency_of_symbol).or_else(|| currency_of_code(code));
                say_money(&a, known, if a.symbol.is_some() { "" } else { code })
            }
            Some(a) if a.negative => {
                let digits = value.trim().trim_start_matches(['(', '-', '\u{2212}']).trim_end_matches(')');
                format!("minus {digits}")
            }
            _ => value.to_owned(),
        },
        Kind::Text => value.to_owned(),
    }
}

fn row_sentence(
    number: usize,
    row: &[String],
    names: &[String],
    columns: &[Column],
    currency: Option<usize>,
    label: Option<usize>,
) -> String {
    let code = currency.and_then(|c| row.get(c)).map_or("", String::as_str);
    let mut parts = Vec::new();
    for (c, value) in row.iter().enumerate() {
        let Some(column) = columns.get(c) else { continue };
        // Said once at the start instead, or part of each amount.
        let said_elsewhere = column.same_everywhere.is_some() || Some(c) == currency;
        if value.is_empty() || said_elsewhere || Some(c) == label {
            continue;
        }
        parts.push(format!("{} {}", names[c], say(column, value, currency, code)));
    }
    let lead = match label.and_then(|c| row.get(c)) {
        Some(value) => format!("{} {value}", capitalise(&names[0])),
        None => format!("Row {}", number + 1),
    };
    if parts.is_empty() { format!("{lead}.") } else { format!("{lead}: {}.", list(&parts)) }
}

fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRANSACTIONS: &str = "Transaction ID,Date,Description,Category,Type,Amount,Currency,Balance
TXN-0001,2026-01-01,Salary,Income,Income,2850.00,GBP,6100.00
TXN-0002,2026-01-03,Rent,Housing,Expense,-950.00,GBP,5150.00
TXN-0004,2026-01-07,Electricity,Utilities,Expense,-68.24,GBP,4831.76
";

    #[test]
    fn reads_a_bank_statement_naturally() {
        let spoken = speakable(TRANSACTIONS).unwrap();
        let lines: Vec<&str> = spoken.lines().collect();
        assert_eq!(
            lines[0],
            "This table has 3 rows, with the columns transaction ID, date, description, category, type, amount, \
             currency and balance. All amounts are in pounds."
        );
        assert_eq!(
            lines[1],
            "Transaction ID TXN-0001: date 1 January 2026, description Salary, category Income, type Income, amount \
             2,850 pounds and balance 6,100 pounds."
        );
        assert_eq!(
            lines[2],
            "Transaction ID TXN-0002: date 3 January 2026, description Rent, category Housing, type Expense, amount \
             minus 950 pounds and balance 5,150 pounds."
        );
        assert!(lines[3].contains("amount minus 68 pounds 24 pence and balance 4,831 pounds 76 pence."), "{}", lines[3]);
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn quoted_cells_can_hold_commas_quotes_and_line_breaks() {
        let rows = parse("Name,Note\r\n\"Smith, Jo\",\"Said \"\"hi\"\"\nthen left\"\r\n\r\nLee,\n");
        assert_eq!(rows, [vec!["Name", "Note"], vec!["Smith, Jo", "Said \"hi\" then left"], vec!["Lee", ""]]);
    }

    #[test]
    fn semicolons_and_tabs_separate_cells_when_the_first_line_says_so() {
        assert_eq!(parse("a;b;c\n1,5;2;3")[1], ["1,5", "2", "3"]);
        assert_eq!(parse("a\tb\n1\t2")[1], ["1", "2"]);
        assert_eq!(parse("\u{feff}a,b\n1,2")[0], ["a", "b"]);
    }

    #[test]
    fn a_table_without_a_header_numbers_its_rows() {
        let spoken = speakable("1,2\n3,4\n").unwrap();
        assert_eq!(spoken, "This table has 2 rows, each with 2 columns.\nRow 1: column 1 1 and column 2 2.\nRow 2: column 1 3 and column 2 4.");
    }

    #[test]
    fn empty_cells_are_left_out_and_shared_values_said_once() {
        let spoken = speakable("Name,Team,Score,Notes\nAmy,Red,10,\nBen,Red,-3,Late\nCat,Red,7,\n").unwrap();
        assert!(spoken.starts_with("This table has 3 rows, with the columns name, team, score and notes. In every row, the team is Red."));
        assert!(spoken.contains("\nName Amy: score 10.\n"), "{spoken}");
        assert!(spoken.contains("\nName Ben: score minus 3 and notes Late.\n"), "{spoken}");
    }

    #[test]
    fn money_with_symbols_and_mixed_currencies() {
        let spoken = speakable("Item,Price\nTea,£1.00\nCake,£0.05\nCoat,\"(£1,250.50)\"\n").unwrap();
        assert!(spoken.contains("Item Tea: price 1 pound."), "{spoken}");
        assert!(spoken.contains("Item Cake: price 5 pence."), "{spoken}");
        assert!(spoken.contains("Item Coat: price minus 1,250 pounds 50 pence."), "{spoken}");

        let spoken = speakable("Ref,Amount,Currency\nA,1.01,USD\nB,2.50,EUR\nC,3.00,CHF\n").unwrap();
        assert!(spoken.contains("Ref A: amount 1 dollar 1 cent."), "{spoken}");
        assert!(spoken.contains("Ref B: amount 2 euros 50 cents."), "{spoken}");
        assert!(spoken.contains("Ref C: amount 3.00 CHF."), "{spoken}");
    }

    #[test]
    fn two_decimal_places_alone_are_not_money() {
        let spoken = speakable("Ref,Weight\nA,1.25\nB,-0.50\n").unwrap();
        assert!(spoken.contains("Ref A: weight 1.25."), "{spoken}");
        assert!(spoken.contains("Ref B: weight minus 0.50."), "{spoken}");
    }

    #[test]
    fn dates_and_times() {
        assert_eq!(date("2026-11-06T17:04:05Z").map(say_date).as_deref(), Some("6 November 2026 at 17:04"));
        assert_eq!(date("2026-02-28").map(say_date).as_deref(), Some("28 February 2026"));
        assert_eq!(date("2026-13-01"), None);
        assert_eq!(date("26-01-01"), None);
        assert_eq!(date("2026-01-01T1"), None);
    }

    #[test]
    fn numbers_are_only_numbers_when_they_look_like_them() {
        assert_eq!(amount("1,234.5").map(|a| a.whole), Some("1234".to_owned()));
        assert!(amount("1,2").is_none());
        assert!(amount("12a").is_none());
        assert!(amount("").is_none());
        assert!(amount("£").is_none());
        assert!(amount("-£5").is_some_and(|a| a.negative && a.symbol == Some('£')));
        assert_eq!(thousands("0001234567"), "1,234,567");
        assert_eq!(thousands("000"), "0");
    }

    #[test]
    fn column_names_read_well_mid_sentence() {
        assert_eq!(column_name("Date"), "date");
        assert_eq!(column_name("unit_price"), "unit price");
        assert_eq!(column_name("ID"), "ID");
        assert_eq!(column_name("  Transaction   ID "), "transaction ID");
    }

    #[test]
    fn nothing_in_it_is_an_error() {
        assert!(speakable("").is_err());
        assert!(speakable(",,\n\n").is_err());
    }

    #[test]
    fn a_single_line_is_read_as_a_row() {
        assert_eq!(speakable("Hello,world").unwrap(), "This table has 1 row, each with 2 columns.\nRow 1: column 1 Hello and column 2 world.");
    }
}
