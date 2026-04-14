//! Small shared helpers: CSV writing, template rendering, output directories, and error
//! text for I/O and database failures.

use std::fs;
use std::io;
use std::path::Path;

use anyhow::anyhow;

/// CSV rows ending in `\n`, with fields quoted only when they need it.
#[derive(Debug, Default)]
pub struct Csv {
    out: String,
}

impl Csv {
    pub fn new() -> Csv {
        Csv::default()
    }

    pub fn row<S: AsRef<str>>(&mut self, fields: &[S]) {
        if fields.len() == 1 && fields[0].as_ref().is_empty() {
            // A lone empty field is quoted so the row isn't mistaken for a blank line.
            self.out.push_str("\"\"\n");
            return;
        }
        for (i, f) in fields.iter().enumerate() {
            if i > 0 {
                self.out.push(',');
            }
            let f = f.as_ref();
            if f.contains([',', '"', '\n', '\r']) {
                self.out.push('"');
                self.out.push_str(&f.replace('"', "\"\""));
                self.out.push('"');
            } else {
                self.out.push_str(f);
            }
        }
        self.out.push('\n');
    }

    pub fn into_string(self) -> String {
        self.out
    }

    /// Write the rows to `path`, replacing any existing file.
    pub fn save(self, path: &str) -> io::Result<()> {
        fs::write(path, self.out)
    }
}

/// Write a CSV file with a header and rows.
pub fn write_csv(path: &str, header: &[&str], rows: &[Vec<String>]) -> io::Result<()> {
    let mut w = Csv::new();
    w.row(header);
    for r in rows {
        w.row(r);
    }
    w.save(path)
}

/// Substitute every `@@NAME@@` token (NAME is `[A-Z_]+`) in one pass, so injected data is
/// never rescanned. Unknown names are left as they are.
pub fn render(template: &str, values: &[(&str, &str)]) -> String {
    let b = template.as_bytes();
    let mut out = String::with_capacity(template.len() * 2);
    let mut last = 0;
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b'@' && b[i + 1] == b'@' {
            let mut j = i + 2;
            while j < b.len() && (b[j].is_ascii_uppercase() || b[j] == b'_') {
                j += 1;
            }
            if j > i + 2 && j + 1 < b.len() && b[j] == b'@' && b[j + 1] == b'@' {
                let name = &template[i + 2..j];
                if let Some((_, v)) = values.iter().find(|(k, _)| *k == name) {
                    out.push_str(&template[last..i]);
                    out.push_str(v);
                    i = j + 2;
                    last = i;
                    continue;
                }
            }
        }
        i += 1;
    }
    out.push_str(&template[last..]);
    out
}

/// Create `dir` and any missing parents with mode 0755; an existing directory is fine.
pub fn make_dirs(dir: &str) -> io::Result<()> {
    let mut b = fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o755);
    }
    match b.create(dir) {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && Path::new(dir).is_dir() => Ok(()),
        r => r,
    }
}

/// Join a directory and a relative name with `/` unless `dir` is empty or already ends in one.
pub fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() || dir.ends_with('/') { format!("{dir}{name}") } else { format!("{dir}/{name}") }
}

/// An I/O error on a path: `path: No such file or directory (os error 2)`.
pub fn os_err(e: &io::Error, path: &str) -> String {
    format!("{path}: {e}")
}

/// A database error reduced to the server's message.
pub fn pg(e: anyhow::Error) -> anyhow::Error {
    if let Some(pe) = e.downcast_ref::<postgres::Error>() {
        if let Some(db) = pe.as_db_error() {
            return anyhow!("{}", db.message());
        }
        return anyhow!("{pe}");
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_quotes_minimally() {
        let mut w = Csv::new();
        w.row(&["a,b", "q\"x", "l\nm", "c\rr", " sp", "", "tab\tx", "é\u{2028}"]);
        w.row(&[""]);
        let empty: [&str; 0] = [];
        w.row(&empty);
        assert_eq!(w.into_string(), "\"a,b\",\"q\"\"x\",\"l\nm\",\"c\rr\", sp,,tab\tx,é\u{2028}\n\"\"\n\n");
    }

    #[test]
    fn render_substitutes_once() {
        let t = "a @@X@@ b @@Y@@ @@lower@@ @@@X@@ @@";
        let got = render(t, &[("X", "@@Y@@"), ("Y", "y")]);
        assert_eq!(got, "a @@Y@@ b y @@lower@@ @@@Y@@ @@");
    }

    #[test]
    fn os_errors_name_the_path() {
        let e = io::Error::from_raw_os_error(2);
        assert_eq!(os_err(&e, "x/y.csv"), "x/y.csv: No such file or directory (os error 2)");
        assert_eq!(join("out", "a.csv"), "out/a.csv");
        assert_eq!(join("out/", "a.csv"), "out/a.csv");
    }
}
