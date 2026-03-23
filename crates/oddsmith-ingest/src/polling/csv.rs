//! A CSV reader: comma delimiter, whitespace after a delimiter skipped, `"` quoting with
//! doubled quotes, lenient about stray quotes, line breaks kept inside quoted fields, and a
//! 131072-character field limit.

use std::fmt;

/// Longest field accepted, in characters.
pub const FIELD_LIMIT: usize = 131_072;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsvError(pub String);

impl fmt::Display for CsvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CsvError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    StartRecord,
    StartField,
    InField,
    InQuotedField,
    QuoteInQuotedField,
    EatCrnl,
}

/// Split text into lines without translating line endings: each line keeps its terminator,
/// which is `\n`, `\r\n` or a lone `\r`.
pub fn split_lines(text: &str) -> Vec<&str> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\n' => {
                out.push(&text[start..=i]);
                start = i + 1;
            }
            b'\r' => {
                let end = if b.get(i + 1) == Some(&b'\n') { i + 1 } else { i };
                out.push(&text[start..=end]);
                start = end + 1;
                i = end;
            }
            _ => {}
        }
        i += 1;
    }
    if start < b.len() {
        out.push(&text[start..]);
    }
    out
}

struct Reader {
    state: State,
    fields: Vec<String>,
    field: String,
    field_len: usize,
}

impl Reader {
    fn save_field(&mut self) {
        self.fields.push(std::mem::take(&mut self.field));
        self.field_len = 0;
    }

    fn add_char(&mut self, c: char) -> Result<(), CsvError> {
        if self.field_len >= FIELD_LIMIT {
            return Err(CsvError(format!("field larger than field limit ({FIELD_LIMIT})")));
        }
        self.field.push(c);
        self.field_len += 1;
        Ok(())
    }

    /// Process one character; `None` marks the end of a line.
    fn process(&mut self, c: Option<char>) -> Result<(), CsvError> {
        let eol = c.is_none();
        let newline = matches!(c, Some('\n' | '\r'));
        if self.state == State::StartRecord {
            if eol {
                return Ok(());
            }
            if newline {
                self.state = State::EatCrnl;
                return Ok(());
            }
            self.state = State::StartField;
        }
        match self.state {
            State::StartRecord => {}
            State::StartField => match c {
                None | Some('\n' | '\r') => {
                    self.save_field();
                    self.state = if eol { State::StartRecord } else { State::EatCrnl };
                }
                Some('"') => self.state = State::InQuotedField,
                Some(' ') => {}
                Some(',') => self.save_field(),
                Some(c) => {
                    self.add_char(c)?;
                    self.state = State::InField;
                }
            },
            State::InField => match c {
                None | Some('\n' | '\r') => {
                    self.save_field();
                    self.state = if eol { State::StartRecord } else { State::EatCrnl };
                }
                Some(',') => {
                    self.save_field();
                    self.state = State::StartField;
                }
                Some(c) => self.add_char(c)?,
            },
            State::InQuotedField => match c {
                None => {}
                Some('"') => self.state = State::QuoteInQuotedField,
                Some(c) => self.add_char(c)?,
            },
            State::QuoteInQuotedField => match c {
                Some('"') => {
                    self.add_char('"')?;
                    self.state = State::InQuotedField;
                }
                Some(',') => {
                    self.save_field();
                    self.state = State::StartField;
                }
                None | Some('\n' | '\r') => {
                    self.save_field();
                    self.state = if eol { State::StartRecord } else { State::EatCrnl };
                }
                Some(c) => {
                    self.add_char(c)?;
                    self.state = State::InField;
                }
            },
            State::EatCrnl => match c {
                Some('\n' | '\r') => {}
                None => self.state = State::StartRecord,
                Some(_) => {
                    return Err(CsvError("new-line character seen in unquoted field".into()));
                }
            },
        }
        Ok(())
    }
}

/// All records in `text`, including empty ones for blank lines.
pub fn read_records(text: &str) -> Result<Vec<Vec<String>>, CsvError> {
    let mut r = Reader { state: State::StartRecord, fields: Vec::new(), field: String::new(), field_len: 0 };
    let mut records = Vec::new();
    for line in split_lines(text) {
        for c in line.chars() {
            r.process(Some(c))?;
        }
        r.process(None)?;
        if r.state == State::StartRecord {
            records.push(std::mem::take(&mut r.fields));
        }
    }
    if r.field_len != 0 || r.state == State::InQuotedField {
        r.save_field();
        records.push(std::mem::take(&mut r.fields));
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(s: &str) -> Vec<Vec<String>> {
        read_records(s).unwrap()
    }

    fn v(items: &[&[&str]]) -> Vec<Vec<String>> {
        items.iter().map(|r| r.iter().map(|s| s.to_string()).collect()).collect()
    }

    /// Quoted fields, stray quotes, blank lines and every line-ending style.
    #[test]
    fn lenient_reader_skips_initial_space() {
        assert_eq!(rows("a,b,c\n1,2,3\n"), v(&[&["a", "b", "c"], &["1", "2", "3"]]));
        assert_eq!(rows("a, b,  c\r\n"), v(&[&["a", "b", "c"]]));
        assert_eq!(rows("\n\na\n\r\n"), v(&[&[], &[], &["a"], &[]]));
        assert_eq!(rows("\"x, y\",\"he said \"\"hi\"\"\"\n"), v(&[&["x, y", "he said \"hi\""]]));
        assert_eq!(rows("\"multi\nline\",2\n"), v(&[&["multi\nline", "2"]]));
        assert_eq!(rows("\"ab\"cd,e\n"), v(&[&["abcd", "e"]]));
        assert_eq!(rows("ab\"cd\",e\n"), v(&[&["ab\"cd\"", "e"]]));
        assert_eq!(rows("a,\n"), v(&[&["a", ""]]));
        assert_eq!(rows("a,b"), v(&[&["a", "b"]]));
        assert_eq!(rows("a,\"unterminated"), v(&[&["a", "unterminated"]]));
        assert_eq!(rows("a,\""), v(&[&["a", ""]]));
        assert_eq!(rows("a\rb\r"), v(&[&["a"], &["b"]]));
        assert_eq!(rows(" , \"q\" ,x\n"), v(&[&["", "q ", "x"]]));
        assert_eq!(rows("\"a\r\nb\"\r\n"), v(&[&["a\r\nb"]]));
    }

    #[test]
    fn field_limit() {
        let big = "x".repeat(FIELD_LIMIT);
        assert_eq!(rows(&big)[0][0].len(), FIELD_LIMIT);
        let too_big = "x".repeat(FIELD_LIMIT + 1);
        assert_eq!(read_records(&too_big).unwrap_err().0, "field larger than field limit (131072)");
    }

    #[test]
    fn line_splitting() {
        assert_eq!(split_lines("a\nb\r\nc\rd"), vec!["a\n", "b\r\n", "c\r", "d"]);
        assert_eq!(split_lines(""), Vec::<&str>::new());
    }
}
