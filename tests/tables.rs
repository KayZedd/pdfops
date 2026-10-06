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

#[test]
fn cells_centred_on_rows_of_several_lines_make_one_row_each() {
    // A comparison as a word processor sets it: room between the rows, and a cell of
    // one line standing half a line below the first line of the cell of two beside it.
    let sheet = Sheet::default()
        .left(72.0, 726.0, "No.")
        .left(110.0, 726.0, "Requirement")
        .left(300.0, 726.0, "Offered")
        .left(420.0, 726.0, "Verdict")
        .left(72.0, 700.0, "1")
        .left(110.0, 700.0, "Uncoiler motor")
        .left(300.0, 700.0, "55 kW")
        .left(420.0, 700.0, "MEETS")
        .left(110.0, 674.0, "Motor of the main")
        .left(420.0, 674.0, "FAILS")
        .left(72.0, 666.0, "2")
        .left(300.0, 666.0, "160 kW")
        .left(110.0, 658.0, "cutting head")
        .left(420.0, 658.0, "by 40 kW")
        .left(110.0, 632.0, "Recoilers for the")
        .left(300.0, 632.0, "11 kW each,")
        .left(72.0, 624.0, "3")
        .left(420.0, 624.0, "MEETS")
        .left(110.0, 616.0, "edge trim")
        .left(300.0, 616.0, "two of them")
        .left(72.0, 590.0, "4")
        .left(110.0, 590.0, "Protective film unit")
        .left(300.0, 590.0, "Not offered")
        .left(420.0, 590.0, "FAILS");
    let found = tables(&[&sheet]);
    assert_eq!(found.as_array().unwrap().len(), 1, "{found}");
    assert_eq!(
        found[0]["cells"],
        json!([
            ["No.", "Requirement", "Offered", "Verdict"],
            ["1", "Uncoiler motor", "55 kW", "MEETS"],
            [
                "2",
                "Motor of the main cutting head",
                "160 kW",
                "FAILS by 40 kW"
            ],
            [
                "3",
                "Recoilers for the edge trim",
                "11 kW each, two of them",
                "MEETS"
            ],
            ["4", "Protective film unit", "Not offered", "FAILS"]
        ])
    );
}

#[test]
fn labels_of_a_drawing_beside_a_list_do_not_join_its_rows() {
    // Names and values, and to their left a dimension written into a sketch, which
    // makes a first column that the other rows have nothing in.
    let sheet = Sheet::default()
        .left(300.0, 700.0, "Developed width")
        .right(540.0, 700.0, "40")
        .left(120.0, 683.0, "90")
        .left(300.0, 683.0, "Sheet width")
        .right(540.0, 683.0, "1100")
        .left(300.0, 666.0, "Strips per sheet")
        .right(540.0, 666.0, "2")
        .left(300.0, 649.0, "Offcut")
        .right(540.0, 649.0, "300");
    let found = tables(&[&sheet]);
    assert_eq!(found.as_array().unwrap().len(), 1, "{found}");
    let pairs: Vec<(&str, &str)> = found[0]["cells"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let row = row.as_array().unwrap();
            (
                row[row.len() - 2].as_str().unwrap(),
                row[row.len() - 1].as_str().unwrap(),
            )
        })
        .filter(|(name, _)| !name.is_empty())
        .collect();
    assert_eq!(
        pairs,
        [
            ("Developed width", "40"),
            ("Sheet width", "1100"),
            ("Strips per sheet", "2"),
            ("Offcut", "300")
        ],
        "{found}"
    );
}

#[test]
fn two_columns_of_running_text_and_lists_are_not_tables() {
    let mut columns = Sheet::default();
    let left = [
        "Before writing any code, decide which two or three",
        "tasks the tool has to make easier for the people",
        "who will use it, and write each of them down as a",
        "request somebody would really type on a busy day",
        "with nothing else to go by than what is on screen.",
    ];
    let right = [
        "Three kinds of use have turned up again and again",
        "in the tools that people kept using after a month,",
        "and each of them asks for a different way of saying",
        "what the tool does and when it should be reached for",
        "instead of something simpler that is already there.",
    ];
    for (i, (a, b)) in left.iter().zip(right).enumerate() {
        let y = 700.0 - 14.0 * i as f64;
        columns = columns.left(72.0, y, a).left(330.0, y, b);
    }
    assert_eq!(tables(&[&columns]), json!([]));

    let mut list = Sheet::default();
    let items = [
        ("1.", "The warranty covers the coating and its colour."),
        ("2.", "Fitting has to follow the maker's instructions."),
        (
            "3.",
            "A defect found within the term is put right at no cost.",
        ),
    ];
    for (i, (number, item)) in items.iter().enumerate() {
        let y = 700.0 - 14.0 * i as f64;
        list = list.left(72.0, y, number).left(96.0, y, item);
    }
    assert_eq!(tables(&[&list]), json!([]));
}
