//! Tables without ruling lines, as financial statements, price lists and reports set
//! them: a label column, figures aligned on the right, labels that wrap, cells left
//! empty. Each case states the cells a reader would name.

mod common;

use common::{call, custom};
use serde_json::{Value, json};

/// Width in points of text made of figures and the signs that go with them, in Helvetica.
fn width(text: &str, size: f64) -> f64 {
    let units: u32 = text
        .chars()
        .map(|c| match c {
            '0'..='9' | '$' => 556,
            ',' | '.' | ' ' => 278,
            '(' | ')' | '-' => 333,
            '%' => 889,
            other => panic!("no width for {other:?}"),
        })
        .sum();
    units as f64 * size / 1000.0
}

/// Builds a page of 10 point text from pieces placed by their left or right edge.
#[derive(Default)]
struct Sheet(String);

impl Sheet {
    fn left(mut self, x: f64, y: f64, text: &str) -> Self {
        self.0 += &format!("1 0 0 1 {x:.2} {y:.2} Tm ({text}) Tj\n");
        self
    }

    /// Figures ending at `x`, as a column of numbers is set.
    fn right(self, x: f64, y: f64, text: &str) -> Self {
        let from = x - width(text, 10.0);
        let escaped = text.replace('(', "\\(").replace(')', "\\)");
        self.left(from, y, &escaped)
    }

    fn content(&self) -> String {
        format!("BT /F1 10 Tf\n{}ET", self.0)
    }
}

fn tables(sheets: &[&Sheet]) -> Value {
    let dir = tempfile::tempdir().unwrap();
    let contents: Vec<String> = sheets.iter().map(|s| s.content()).collect();
    let refs: Vec<&str> = contents.iter().map(String::as_str).collect();
    let v = call(
        "pdf_tables",
        json!({"input": custom(dir.path(), "t.pdf", &refs)}),
    );
    v["tables"].clone()
}

#[test]
fn a_statement_with_wrapped_labels_section_headings_and_negative_figures() {
    let sheet = Sheet::default()
        .right(400.0, 700.0, "2024")
        .right(500.0, 700.0, "2023")
        .left(72.0, 684.0, "Revenue")
        .left(82.0, 670.0, "Product sales")
        .right(400.0, 670.0, "12,450")
        .right(500.0, 670.0, "11,200")
        .left(82.0, 656.0, "Services and other recurring")
        .left(92.0, 644.0, "income")
        .right(400.0, 644.0, "3,310")
        .right(500.0, 644.0, "2,875")
        .left(72.0, 628.0, "Cost of sales")
        .right(400.0, 628.0, "(9,120)")
        .right(500.0, 628.0, "(8,640)")
        .left(72.0, 614.0, "Gross profit")
        .right(400.0, 614.0, "6,640")
        .right(500.0, 614.0, "5,435");
    let found = tables(&[&sheet]);
    assert_eq!(found.as_array().unwrap().len(), 1, "{found}");
    assert_eq!(found[0]["detected_by"], "alignment");
    assert_eq!(
        found[0]["cells"],
        json!([
            ["", "2024", "2023"],
            ["Revenue", "", ""],
            ["Product sales", "12,450", "11,200"],
            ["Services and other recurring income", "3,310", "2,875"],
            ["Cost of sales", "(9,120)", "(8,640)"],
            ["Gross profit", "6,640", "5,435"]
        ])
    );
}

#[test]
fn a_description_that_wraps_under_an_empty_first_column() {
    let sheet = Sheet::default()
        .left(72.0, 700.0, "No.")
        .left(110.0, 700.0, "Description")
        .left(404.44, 700.0, "Qty")
        .left(477.22, 700.0, "Price")
        .left(72.0, 686.0, "1")
        .left(110.0, 686.0, "Steel bracket, galvanised, with")
        .right(420.0, 686.0, "40")
        .right(500.0, 686.0, "12.50")
        .left(110.0, 674.0, "mounting holes on both sides")
        .left(72.0, 660.0, "2")
        .left(110.0, 660.0, "Washer")
        .right(420.0, 660.0, "800")
        .right(500.0, 660.0, "0.04")
        .left(72.0, 646.0, "3")
        .left(110.0, 646.0, "Anchor bolt")
        .right(420.0, 646.0, "16")
        .right(500.0, 646.0, "3.10");
    let found = tables(&[&sheet]);
    assert_eq!(found.as_array().unwrap().len(), 1, "{found}");
    assert_eq!(
        found[0]["cells"],
        json!([
            ["No.", "Description", "Qty", "Price"],
            [
                "1",
                "Steel bracket, galvanised, with mounting holes on both sides",
                "40",
                "12.50"
            ],
            ["2", "Washer", "800", "0.04"],
            ["3", "Anchor bolt", "16", "3.10"]
        ])
    );
}

#[test]
fn rows_with_empty_cells_and_a_row_holding_one_figure_stay_in_the_table() {
    let sheet = Sheet::default()
        .left(72.0, 700.0, "Region")
        .right(300.0, 700.0, "2022")
        .right(400.0, 700.0, "2023")
        .right(500.0, 700.0, "2024")
        .left(72.0, 686.0, "North")
        .right(300.0, 686.0, "120")
        .right(500.0, 686.0, "140")
        .left(72.0, 672.0, "South")
        .right(400.0, 672.0, "95")
        .right(500.0, 672.0, "101")
        // A total set alone under the last column.
        .right(500.0, 658.0, "241")
        .left(72.0, 644.0, "East")
        .right(300.0, 644.0, "60")
        .right(400.0, 644.0, "64")
        .right(500.0, 644.0, "70");
    let found = tables(&[&sheet]);
    assert_eq!(found.as_array().unwrap().len(), 1, "{found}");
    assert_eq!(
        found[0]["cells"],
        json!([
            ["Region", "2022", "2023", "2024"],
            ["North", "120", "", "140"],
            ["South", "", "95", "101"],
            ["", "", "", "241"],
            ["East", "60", "64", "70"]
        ])
    );
}

#[test]
fn a_heading_across_two_columns_does_not_join_them() {
    let sheet = Sheet::default()
        .left(330.0, 714.0, "Year ended 31 December")
        .right(400.0, 700.0, "2024")
        .right(500.0, 700.0, "2023")
        .left(72.0, 686.0, "Cash")
        .right(400.0, 686.0, "1,200")
        .right(500.0, 686.0, "980")
        .left(72.0, 672.0, "Receivables")
        .right(400.0, 672.0, "430")
        .right(500.0, 672.0, "515")
        .left(72.0, 658.0, "Inventory")
        .right(400.0, 658.0, "2,015")
        .right(500.0, 658.0, "1,870")
        .left(72.0, 644.0, "Total")
        .right(400.0, 644.0, "3,645")
        .right(500.0, 644.0, "3,365");
    let found = tables(&[&sheet]);
    let table = found
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["cells"].to_string().contains("Receivables"))
        .unwrap_or_else(|| panic!("{found}"));
    assert_eq!(table["columns"], 3, "{table}");
    let cells = table["cells"].as_array().unwrap();
    assert_eq!(
        cells[cells.len() - 4..],
        [
            json!(["Cash", "1,200", "980"]),
            json!(["Receivables", "430", "515"]),
            json!(["Inventory", "2,015", "1,870"]),
            json!(["Total", "3,645", "3,365"])
        ]
    );
}

#[test]
fn a_table_of_contents_with_dot_leaders_is_not_a_table() {
    let mut sheet = Sheet::default();
    for (i, (title, page)) in [
        ("Introduction", "1"),
        ("Scope of the agreement", "4"),
        ("Definitions", "9"),
        ("Payment terms", "15"),
        ("Termination", "22"),
    ]
    .iter()
    .enumerate()
    {
        let y = 700.0 - 14.0 * i as f64;
        sheet = sheet
            .left(72.0, y, title)
            .left(
                200.0,
                y,
                ". . . . . . . . . . . . . . . . . . . . . . . . . . . . . .",
            )
            .right(500.0, y, page);
    }
    assert_eq!(tables(&[&sheet]), json!([]));
    // Leaders set as an unbroken run of full stops.
    let mut sheet = Sheet::default();
    for (i, title) in ["Introduction", "Definitions", "Payment terms"]
        .iter()
        .enumerate()
    {
        let y = 700.0 - 14.0 * i as f64;
        sheet = sheet
            .left(
                72.0,
                y,
                &format!("{title} ......................................"),
            )
            .right(500.0, y, &(3 * i + 1).to_string());
    }
    assert_eq!(tables(&[&sheet]), json!([]));
}

#[test]
fn text_between_two_tables_belongs_to_neither() {
    let sheet = Sheet::default()
        .left(72.0, 700.0, "Bolt M8")
        .right(300.0, 700.0, "40")
        .left(72.0, 686.0, "Washer")
        .right(300.0, 686.0, "80")
        .left(72.0, 672.0, "Nut M8")
        .right(300.0, 672.0, "40")
        .left(
            72.0,
            650.0,
            "The quantities above are per assembly and include spares for the first service.",
        )
        .left(72.0, 628.0, "Paint, grey")
        .right(300.0, 628.0, "2")
        .left(72.0, 614.0, "Primer")
        .right(300.0, 614.0, "1");
    let found = tables(&[&sheet]);
    assert_eq!(found.as_array().unwrap().len(), 2, "{found}");
    assert_eq!(
        found[0]["cells"],
        json!([["Bolt M8", "40"], ["Washer", "80"], ["Nut M8", "40"]])
    );
    assert_eq!(
        found[1]["cells"],
        json!([["Paint, grey", "2"], ["Primer", "1"]])
    );
}
