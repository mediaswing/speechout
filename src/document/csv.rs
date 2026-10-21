//! CSV files, read as a spoken table, row by row. ODS sheets are read the same way.

use super::odf::row_is_empty;
use super::text::read_txt;

/// Reads a CSV file as a spoken table: how many columns and rows it has, then
/// one paragraph per row giving each column's name and value. The first row
/// holds the column names. Empty cells are left out, so sparse tables do not
/// read as a long list of blanks.
pub(super) fn read_csv(bytes: &[u8]) -> String {
    let text = read_txt(bytes);
    speak_table(parse_csv(&text, detect_delimiter(&text)))
}

/// Reads rows of cells as a spoken table. See `read_csv`.
pub(super) fn speak_table(records: Vec<Vec<String>>) -> String {
    let mut records = records.into_iter().filter(|r| !row_is_empty(r));
    let Some(header) = records.next() else { return String::new() };
    let rows: Vec<Vec<String>> = records.collect();
    let columns = rows.iter().map(Vec::len).fold(header.len(), usize::max);
    let names: Vec<String> = (0..columns)
        .map(|i| match header.get(i).map(|s| s.trim()) {
            Some(name) if !name.is_empty() => speakable_column(name),
            _ => format!("Column {}", i + 1),
        })
        .collect();

    let mut out = format!("Table with {} and {}.", count(columns, "column"), count(rows.len(), "row"));
    if rows.is_empty() {
        out.push_str(&format!(" The columns are {}.", names.join(", ")));
    }
    for (n, row) in rows.iter().enumerate() {
        out.push_str(&format!("\n\nRow {}.", n + 1));
        for (name, cell) in names.iter().zip(row) {
            // Line breaks inside a cell would otherwise split the row in two.
            let cell = cell.split_whitespace().collect::<Vec<_>>().join(" ");
            if !cell.is_empty() {
                out.push_str(&format!(" {name}: {cell}"));
                if !cell.ends_with(['.', '!', '?']) {
                    out.push('.');
                }
            }
        }
    }
    out
}

/// Makes a column name read naturally: "query_type" becomes "query type" and
/// "Totals.Capital outlay" becomes "Totals, Capital outlay". Names whose dots
/// mark abbreviations, such as "No." or "e.g.", keep them.
fn speakable_column(name: &str) -> String {
    let name = name.replace('_', " ");
    let parts: Vec<&str> = name.split('.').map(str::trim).collect();
    if parts.len() > 1 && parts.iter().all(|p| p.chars().count() >= 2) {
        parts.join(", ")
    } else {
        name.trim().to_owned()
    }
}

/// "1 row", "3 rows" or "no rows".
pub(super) fn count(n: usize, noun: &str) -> String {
    match n {
        0 => format!("no {noun}s"),
        1 => format!("1 {noun}"),
        _ => format!("{n} {noun}s"),
    }
}

/// Picks whichever of comma, semicolon or tab appears most often in the first
/// line (outside quotes). Spreadsheets saved in many European locales use
/// semicolons. Commas win a tie.
fn detect_delimiter(text: &str) -> char {
    let mut counts = [(',', 0), (';', 0), ('\t', 0)];
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            '\n' | '\r' if !quoted => break,
            _ if !quoted => {
                if let Some(entry) = counts.iter_mut().find(|(d, _)| *d == c) {
                    entry.1 += 1;
                }
            }
            _ => {}
        }
    }
    counts.iter().rev().max_by_key(|(_, n)| *n).filter(|(_, n)| *n > 0).map_or(',', |(d, _)| *d)
}

/// Splits CSV text into records of fields. Handles quoted fields containing
/// the delimiter, line breaks or doubled quotes, and any style of line ending.
/// A quote part-way through an unquoted field is kept as an ordinary character.
fn parse_csv(text: &str, delimiter: char) -> Vec<Vec<String>> {
    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            if c != '"' {
                field.push(c);
            } else if chars.next_if_eq(&'"').is_some() {
                field.push('"');
            } else {
                quoted = false;
            }
        } else if c == '"' && field.is_empty() {
            quoted = true;
        } else if c == delimiter {
            record.push(std::mem::take(&mut field));
        } else if c == '\n' || c == '\r' {
            if c == '\r' {
                chars.next_if_eq(&'\n');
            }
            record.push(std::mem::take(&mut field));
            records.push(std::mem::take(&mut record));
        } else {
            field.push(c);
        }
    }
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }
    records
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tidy;

    #[test]
    fn csv_reads_as_a_table() {
        let csv = "Name,Age,Town\r\nAnn,41,Leeds\r\n\"Smith, Bob\",,\"York\"\r\n";
        assert_eq!(
            tidy(&read_csv(csv.as_bytes())),
            "Table with 3 columns and 2 rows.\n\n\
             Row 1. Name: Ann. Age: 41. Town: Leeds.\n\n\
             Row 2. Name: Smith, Bob. Town: York."
        );
    }

    #[test]
    fn csv_semicolons_and_quotes() {
        let csv = "Username;Note\nbooker12;\"Said \"\"hi\"\"\n\nthen left.\"\n\njenkins46;x";
        assert_eq!(
            tidy(&read_csv(csv.as_bytes())),
            "Table with 2 columns and 2 rows.\n\n\
             Row 1. Username: booker12. Note: Said \"hi\" then left.\n\n\
             Row 2. Username: jenkins46. Note: x."
        );
    }

    #[test]
    fn csv_unnamed_and_extra_columns() {
        let csv = "a,\n1,2,3\n";
        assert_eq!(
            tidy(&read_csv(csv.as_bytes())),
            "Table with 3 columns and 1 row.\n\nRow 1. a: 1. Column 2: 2. Column 3: 3."
        );
    }

    #[test]
    fn csv_column_names_read_naturally() {
        assert_eq!(speakable_column("query_type"), "query type");
        assert_eq!(speakable_column("Totals.Capital outlay"), "Totals, Capital outlay");
        assert_eq!(speakable_column("Totals. Debt at end"), "Totals, Debt at end");
        assert_eq!(speakable_column("Ref No."), "Ref No.");
        assert_eq!(speakable_column("e.g. size"), "e.g. size");
    }

    #[test]
    fn csv_header_only_or_empty() {
        assert_eq!(read_csv(b"a,b\n"), "Table with 2 columns and no rows. The columns are a, b.");
        assert_eq!(read_csv(b"\n\n"), "");
    }

    #[test]
    fn csv_delimiter_ignores_quoted_text() {
        assert_eq!(detect_delimiter("\"a;b;c\",d\n"), ',');
        assert_eq!(detect_delimiter("a\tb\tc\n"), '\t');
        assert_eq!(detect_delimiter("single"), ',');
    }
}
