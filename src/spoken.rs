//! Rewrites things that voices tend to read badly into the way a person would
//! say them: email addresses, web addresses, IP addresses, ISO dates and
//! times, and long numbers. This runs just before speaking, so the text shown
//! and copied is never changed.

use regex::{Captures, Regex};
use std::sync::OnceLock;

const MONTHS: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November",
    "December",
];

fn patterns() -> &'static [Regex; 5] {
    static PATTERNS: OnceLock<[Regex; 5]> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            // Email address.
            r"\b[A-Za-z0-9][A-Za-z0-9._%+-]*@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,24}\b",
            // ISO date, optionally with a time and time zone.
            r"\b([0-9]{4})-([0-9]{2})-([0-9]{2})(?:[T ]([0-9]{2}):([0-9]{2})(?::([0-9]{2})(\.[0-9]+)?)?(Z|[+-][0-9]{2}:?[0-9]{2})?)?\b",
            // IPv4 address.
            r"\b([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})\b",
            // Domain or file name in lower case, such as api.example.com.
            // Requiring lower case leaves dotted names such as "Totals.Revenue"
            // and abbreviations such as "e.g." alone.
            r"\b[a-z0-9](?:[a-z0-9-]*[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]*[a-z0-9])?)*\.[a-z]{2,24}\b",
            // Run of digits.
            r"[0-9]+",
        ]
        .map(|p| Regex::new(p).expect("valid pattern"))
    })
}

/// The text with every rewrite applied.
pub fn apply(text: &str) -> String {
    let [email, date, ip, domain, number] = patterns();
    let text = replace(text, email, |c, _, _| Some(speak_email(&c[0])));
    let text = replace(&text, date, |c, _, _| speak_date(c));
    let text = replace(&text, ip, speak_ip);
    let text = replace(&text, domain, |c, before, _| {
        (!before.ends_with(['@', '.'])).then(|| c[0].replace('.', " dot "))
    });
    replace(&text, number, |c, before, after| group_digits(&c[0], before, after))
}

/// Replaces each match for which `f` returns a spoken form. `f` also gets the
/// text before and after the match, to check what surrounds it.
fn replace(text: &str, re: &Regex, mut f: impl FnMut(&Captures, &str, &str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for caps in re.captures_iter(text) {
        let m = caps.get(0).expect("whole match");
        if let Some(spoken) = f(&caps, &text[..m.start()], &text[m.end()..]) {
            out.push_str(&text[last..m.start()]);
            out.push_str(&spoken);
            last = m.end();
        }
    }
    out.push_str(&text[last..]);
    out
}

/// "jo.smith@example.com" becomes "jo dot smith at example dot com".
fn speak_email(address: &str) -> String {
    let mut out = String::new();
    for c in address.chars() {
        match c {
            '@' => out.push_str(" at "),
            '.' => out.push_str(" dot "),
            '_' => out.push_str(" underscore "),
            '+' => out.push_str(" plus "),
            _ => out.push(c),
        }
    }
    out
}

/// "2026-10-01T01:33:33.556Z" becomes "1 October 2026 at 01:33:33.556 UTC".
fn speak_date(c: &Captures) -> Option<String> {
    let num = |i: usize| c.get(i).and_then(|m| m.as_str().parse::<u32>().ok());
    let (year, month, day) = (&c[1], num(2)?, num(3)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut out = format!("{day} {} {year}", MONTHS[month as usize - 1]);
    if let (Some(hour), Some(minute)) = (num(4), num(5)) {
        if hour > 23 || minute > 59 || num(6).is_some_and(|s| s > 60) {
            return None;
        }
        out.push_str(&format!(" at {}:{}", &c[4], &c[5]));
        if let Some(seconds) = c.get(6) {
            out.push(':');
            out.push_str(seconds.as_str());
            out.push_str(c.get(7).map_or("", |m| m.as_str()));
        }
        match c.get(8).map(|m| m.as_str()) {
            Some("Z" | "+00:00" | "+0000" | "-00:00" | "-0000") => out.push_str(" UTC"),
            Some(zone) => {
                let (sign, rest) = zone.split_at(1);
                let digits = rest.replace(':', "");
                let word = if sign == "+" { "plus" } else { "minus" };
                out.push_str(&format!(" UTC {word} {}:{}", &digits[..2], &digits[2..]));
            }
            None => {}
        }
    }
    Some(out)
}

/// "51.6.184.166" becomes "51 dot 6 dot 184 dot 166", rather than being read
/// as a decimal number.
fn speak_ip(c: &Captures, before: &str, after: &str) -> Option<String> {
    let parts: Vec<&str> = (1..=4).map(|i| c.get(i).map_or("", |m| m.as_str())).collect();
    let continues = after.starts_with('.') && after[1..].starts_with(|c: char| c.is_ascii_digit());
    if before.ends_with('.') || continues || parts.iter().any(|p| p.parse::<u32>().map_or(true, |n| n > 255)) {
        return None;
    }
    Some(parts.join(" dot "))
}

/// Adds thousands separators to a number of five or more digits, such as
/// 10536166, so every voice reads it as "ten million, five hundred..." rather
/// than digit by digit. Four-digit numbers are left alone so years read
/// naturally. Digits that look like part of a code, phone number, version,
/// time or longer number are left alone too.
fn group_digits(digits: &str, before: &str, after: &str) -> Option<String> {
    if digits.len() < 5 || digits.starts_with('0') {
        return None;
    }
    let mut prev = before.chars().rev();
    match prev.next() {
        Some(c) if c.is_alphanumeric() || ".,_/:#+@\\".contains(c) => return None,
        // A minus sign, but not a hyphen inside a code such as AB-12345.
        Some('-') if prev.next().is_some_and(|c| !c.is_whitespace() && c != '(') => return None,
        // The second group of a phone number, such as 7700 900123.
        Some(c) if c.is_whitespace() && prev.next().is_some_and(|c| c.is_ascii_digit()) => return None,
        _ => {}
    }
    let mut next = after.chars();
    match next.next() {
        Some(c) if c.is_alphanumeric() || "_/:@".contains(c) => return None,
        Some(',' | '-' | ' ') if next.next().is_some_and(|c| c.is_ascii_digit()) => return None,
        _ => {}
    }
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_addresses() {
        assert_eq!(apply("Login email: rachel@example.com."), "Login email: rachel at example dot com.");
        assert_eq!(
            apply("Write to jo.smith_2+news@mail.your-company.co.uk"),
            "Write to jo dot smith underscore 2 plus news at mail dot your-company dot co dot uk"
        );
    }

    #[test]
    fn domains_but_not_dotted_names() {
        assert_eq!(apply("domain: api.nextdns.io."), "domain: api dot nextdns dot io.");
        assert_eq!(apply("Totals.Revenue: 5, e.g. this, i.e. that"), "Totals.Revenue: 5, e.g. this, i.e. that");
        assert_eq!(apply("Open report.pdf now"), "Open report dot pdf now");
        assert_eq!(apply("It ended. then"), "It ended. then");
    }

    #[test]
    fn dates_and_times() {
        assert_eq!(apply("timestamp: 2026-10-01T01:33:33.556Z."), "timestamp: 1 October 2026 at 01:33:33.556 UTC.");
        assert_eq!(apply("on 2019-03-09"), "on 9 March 2019");
        assert_eq!(apply("2026-06-30 18:05+01:00"), "30 June 2026 at 18:05 UTC plus 01:00");
        assert_eq!(apply("code 2026-13-45"), "code 2026-13-45");
    }

    #[test]
    fn ip_addresses() {
        assert_eq!(apply("client_ip: 51.6.184.166."), "client_ip: 51 dot 6 dot 184 dot 166.");
        assert_eq!(apply("version 1.2.3.4.5"), "version 1.2.3.4.5");
        assert_eq!(apply("not 300.1.1.1"), "not 300.1.1.1");
    }

    #[test]
    fn long_numbers_get_separators() {
        assert_eq!(apply("Revenue: 10536166. Year: 1992."), "Revenue: 10,536,166. Year: 1992.");
        assert_eq!(apply("£123456.78 and -99999"), "£123,456.78 and -99,999");
        assert_eq!(apply("(12345)"), "(12,345)");
    }

    #[test]
    fn codes_and_phone_numbers_are_left_alone() {
        for text in [
            "id 655BF", "AB-12345", "07700900123", "+44 7700 900123", "12345-67890", "v12345", "1,234,567",
            "ref#123456", "12345/2026", "10:30:00",
        ] {
            assert_eq!(apply(text), text);
        }
    }
}
