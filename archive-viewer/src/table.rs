//! Minimal aligned plain-text table renderer, shared by `list` and `rollup`.
//! No dependency on a table crate — columns are left-aligned and padded to the
//! widest cell (header included), separated by two spaces.

/// Render `headers` + `rows` as an aligned table string (no trailing newline).
/// Every row is expected to have `headers.len()` cells; short rows are padded
/// with blanks and extra cells are ignored, so a caller mismatch degrades to a
/// readable table rather than a panic.
pub fn render(headers: &[&str], rows: &[Vec<String>]) -> String {
    let cols = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().take(cols).enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }

    let mut out = String::new();
    push_row(
        &mut out,
        &widths,
        &headers.iter().map(|h| h.to_string()).collect::<Vec<_>>(),
    );
    for row in rows {
        out.push('\n');
        push_row(&mut out, &widths, row);
    }
    out
}

fn push_row(out: &mut String, widths: &[usize], cells: &[String]) {
    let empty = String::new();
    let parts: Vec<String> = widths
        .iter()
        .enumerate()
        .map(|(i, &w)| {
            let cell = cells.get(i).unwrap_or(&empty);
            let pad = w.saturating_sub(cell.chars().count());
            format!("{cell}{}", " ".repeat(pad))
        })
        .collect();
    out.push_str(parts.join("  ").trim_end());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligns_columns_to_widest_cell() {
        let out = render(
            &["a", "bbb"],
            &[
                vec!["xx".to_string(), "y".to_string()],
                vec!["z".to_string(), "wwww".to_string()],
            ],
        );
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "a   bbb");
        assert_eq!(lines[1], "xx  y");
        assert_eq!(lines[2], "z   wwww");
    }

    #[test]
    fn short_rows_are_padded_not_panicking() {
        let out = render(&["a", "b", "c"], &[vec!["1".to_string()]]);
        assert_eq!(out.lines().next_back(), Some("1"));
    }
}
