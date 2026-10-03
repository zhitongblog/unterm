use super::*;
use base64::engine::general_purpose::STANDARD;

const GEO: Geometry = Geometry {
    cols: 80,
    rows: 24,
    cell_width: 10,
    cell_height: 20,
    top_row: 100,
};

fn png(width: u32, height: u32) -> Vec<u8> {
    let rgba: Vec<u8> = (0..width * height)
        .flat_map(|i| [(i % 256) as u8, 10, 20, 255])
        .collect();
    decode::png(width, height, &rgba)
}

fn at(row: i64, col: usize) -> At {
    At { row, col }
}

fn reply_text(outcome: &Outcome) -> String {
    String::from_utf8(outcome.reply.clone().unwrap_or_default()).unwrap()
}

// ---------------------------------------------------------------- iTerm2

#[test]
fn an_iterm_image_is_placed_at_the_cursor_and_moves_it_below() {
    let mut images = Images::new();
    let body = format!("File=inline=1:{}", STANDARD.encode(png(40, 60)));
    let outcome = images.iterm(&body, at(105, 2), GEO);

    // 40x60 px in 10x20 cells: 4 columns, 3 rows.
    let shown = images.visible(100, 24);
    assert_eq!(shown.len(), 1);
    assert_eq!(
        (shown[0].row, shown[0].col, shown[0].cols, shown[0].rows),
        (105, 2, 4, 3)
    );
    assert_eq!(shown[0].protocol, ImageProtocol::Iterm);
    assert_eq!(outcome.cursor, Some(CursorMove { down: 2, col: 6 }));
    assert!(outcome.changed);
}

#[test]
fn an_iterm_file_without_inline_is_not_shown() {
    let mut images = Images::new();
    let body = format!("File=name=eA==:{}", STANDARD.encode(png(4, 4)));
    assert_eq!(images.iterm(&body, at(100, 0), GEO), Outcome::default());
    assert!(images.visible(100, 24).is_empty());
}

#[test]
fn an_iterm_image_wider_than_the_screen_is_scaled_to_it() {
    let mut images = Images::new();
    // 1600 px is twice the 800 px screen.
    let body = format!("File=inline=1:{}", STANDARD.encode(png(1600, 400)));
    images.iterm(&body, at(100, 0), GEO);

    let shown = &images.visible(100, 24)[0];
    assert_eq!((shown.cols, shown.rows), (80, 10));
}

#[test]
fn iterm_width_in_cells_keeps_the_shape() {
    let mut images = Images::new();
    let body = format!(
        "File=inline=1;width=10;name={}:{}",
        STANDARD.encode("cat.png"),
        STANDARD.encode(png(40, 40))
    );
    images.iterm(&body, at(100, 0), GEO);

    let shown = &images.visible(100, 24)[0];
    // 10 cells = 100 px wide, so 100 px tall = 5 rows.
    assert_eq!((shown.cols, shown.rows), (10, 5));
    assert_eq!(shown.name.as_deref(), Some("cat.png"));
    assert_eq!(shown.fit, ImageFit::Contain);
}

#[test]
fn a_multipart_iterm_file_is_assembled() {
    let mut images = Images::new();
    let data = STANDARD.encode(png(20, 20));
    let (a, b) = data.split_at(data.len() / 2);
    images.iterm("MultipartFile=inline=1", at(100, 0), GEO);
    images.iterm(&format!("FilePart={a}"), at(100, 0), GEO);
    images.iterm(&format!("FilePart={b}"), at(100, 0), GEO);
    let outcome = images.iterm("FileEnd", at(100, 0), GEO);

    assert!(outcome.changed);
    assert_eq!(images.visible(100, 24).len(), 1);
}

#[test]
fn do_not_move_cursor_is_honoured() {
    let mut images = Images::new();
    let body = format!(
        "File=inline=1;doNotMoveCursor=1:{}",
        STANDARD.encode(png(20, 20))
    );
    assert_eq!(images.iterm(&body, at(100, 0), GEO).cursor, None);
}

// ----------------------------------------------------------------- kitty

fn kitty_png(keys: &str, width: u32, height: u32) -> String {
    format!("G{keys};{}", STANDARD.encode(png(width, height)))
}

#[test]
fn kitty_transmit_and_display_replies_and_places() {
    let mut images = Images::new();
    let outcome = images.kitty(&kitty_png("a=T,f=100,i=7", 30, 40), at(110, 5), GEO);

    assert_eq!(reply_text(&outcome), "\x1b_Gi=7;OK\x1b\\");
    let shown = &images.visible(100, 24)[0];
    assert_eq!((shown.row, shown.col, shown.cols, shown.rows), (110, 5, 3, 2));
    assert_eq!(outcome.cursor, Some(CursorMove { down: 1, col: 8 }));
}

#[test]
fn kitty_without_an_id_says_nothing() {
    let mut images = Images::new();
    let outcome = images.kitty(&kitty_png("a=T,f=100", 10, 10), at(100, 0), GEO);
    assert_eq!(outcome.reply, None);
    assert_eq!(images.visible(100, 24).len(), 1);
}

#[test]
fn kitty_quiet_suppresses_ok_but_not_errors_at_one() {
    let mut images = Images::new();
    let ok = images.kitty(&kitty_png("a=T,f=100,i=1,q=1", 10, 10), at(100, 0), GEO);
    assert_eq!(ok.reply, None);
    let err = images.kitty("Ga=p,i=99,q=1", at(100, 0), GEO);
    assert!(reply_text(&err).contains("ENOENT"), "{}", reply_text(&err));
    let silent = images.kitty("Ga=p,i=99,q=2", at(100, 0), GEO);
    assert_eq!(silent.reply, None);
}

#[test]
fn kitty_chunked_transfer_is_assembled_and_answered_once() {
    let mut images = Images::new();
    let data = STANDARD.encode(png(16, 16));
    let (a, rest) = data.split_at(8);
    let (b, c) = rest.split_at(8);
    assert_eq!(
        images.kitty(&format!("Ga=T,f=100,i=3,m=1;{a}"), at(100, 0), GEO),
        Outcome::default()
    );
    assert_eq!(images.kitty(&format!("Gm=1;{b}"), at(100, 0), GEO), Outcome::default());
    let outcome = images.kitty(&format!("Gm=0;{c}"), at(100, 0), GEO);

    assert_eq!(reply_text(&outcome), "\x1b_Gi=3;OK\x1b\\");
    assert_eq!(images.visible(100, 24).len(), 1);
}

#[test]
fn kitty_query_answers_without_storing() {
    let mut images = Images::new();
    // The probe every kitty client sends: one RGB pixel.
    let outcome = images.kitty("Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA", at(100, 0), GEO);
    assert_eq!(reply_text(&outcome), "\x1b_Gi=31;OK\x1b\\");
    assert!(images.is_empty());
}

#[test]
fn kitty_raw_rgba_needs_its_size() {
    let mut images = Images::new();
    let pixels = STANDARD.encode([255u8, 0, 0, 255, 0, 255, 0, 255]);
    let ok = images.kitty(&format!("Ga=T,f=32,s=2,v=1,i=4;{pixels}"), at(100, 0), GEO);
    assert!(reply_text(&ok).ends_with(";OK\x1b\\"));
    let bad = images.kitty(&format!("Ga=T,f=32,s=9,v=9,i=5;{pixels}"), at(100, 0), GEO);
    assert!(reply_text(&bad).contains("EBADF"), "{}", reply_text(&bad));
}

#[test]
fn kitty_zlib_compressed_pixels() {
    use std::io::Write;
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&[9u8, 9, 9, 255]).unwrap();
    let compressed = STANDARD.encode(encoder.finish().unwrap());
    let mut images = Images::new();
    let outcome = images.kitty(&format!("Ga=T,f=32,o=z,s=1,v=1,i=6;{compressed}"), at(100, 0), GEO);
    assert!(reply_text(&outcome).ends_with(";OK\x1b\\"), "{}", reply_text(&outcome));
    let key = images.visible(100, 24)[0].image.clone();
    assert_eq!(images.data(&key).unwrap().rgba, [9, 9, 9, 255]);
}

#[test]
fn kitty_put_places_a_stored_image_again() {
    let mut images = Images::new();
    images.kitty(&kitty_png("a=t,f=100,i=2", 10, 10), at(100, 0), GEO);
    assert!(images.visible(100, 24).is_empty(), "transmit alone shows nothing");

    let outcome = images.kitty("Ga=p,i=2,c=4,r=2", at(103, 1), GEO);
    assert_eq!(reply_text(&outcome), "\x1b_Gi=2;OK\x1b\\");
    let shown = &images.visible(100, 24)[0];
    assert_eq!((shown.cols, shown.rows, shown.fit), (4, 2, ImageFit::Fill));
}

#[test]
fn a_placement_id_replaces_the_previous_placement() {
    let mut images = Images::new();
    images.kitty(&kitty_png("a=t,f=100,i=2", 10, 10), at(100, 0), GEO);
    images.kitty("Ga=p,i=2,p=1", at(100, 0), GEO);
    images.kitty("Ga=p,i=2,p=1", at(105, 0), GEO);
    images.kitty("Ga=p,i=2,p=2", at(108, 0), GEO);

    let rows: Vec<i64> = images.visible(100, 24).iter().map(|p| p.row).collect();
    assert_eq!(rows, [105, 108]);
}

#[test]
fn kitty_image_number_gets_an_id_in_the_reply() {
    let mut images = Images::new();
    let outcome = images.kitty(&kitty_png("a=t,f=100,I=13", 10, 10), at(100, 0), GEO);
    let text = reply_text(&outcome);
    assert!(text.contains("I=13") && text.contains("i="), "{text}");
}

#[test]
fn kitty_source_rectangle_is_kept() {
    let mut images = Images::new();
    images.kitty(&kitty_png("a=T,f=100,i=8,x=10,y=0,w=20,h=40", 60, 40), at(100, 0), GEO);
    let shown = &images.visible(100, 24)[0];
    assert_eq!(shown.source, Some([10, 0, 20, 40]));
    assert_eq!((shown.cols, shown.rows), (2, 2));
}

#[test]
fn kitty_delete_by_id_and_free() {
    let mut images = Images::new();
    images.kitty(&kitty_png("a=T,f=100,i=1", 10, 10), at(100, 0), GEO);
    images.kitty(&kitty_png("a=T,f=100,i=2", 12, 10), at(101, 0), GEO);

    assert!(images.kitty("Ga=d,d=i,i=1", at(100, 0), GEO).changed);
    assert_eq!(images.visible(100, 24).len(), 1);
    // Lowercase keeps the data: it can be placed again.
    assert!(images.kitty("Ga=p,i=1", at(102, 0), GEO).reply.is_some());
    // Uppercase frees it.
    images.kitty("Ga=d,d=I,i=1", at(100, 0), GEO);
    assert!(reply_text(&images.kitty("Ga=p,i=1", at(102, 0), GEO)).contains("ENOENT"));
}

#[test]
fn kitty_delete_all_only_touches_the_visible_screen() {
    let mut images = Images::new();
    images.kitty(&kitty_png("a=T,f=100", 10, 10), at(50, 0), GEO);
    images.kitty(&kitty_png("a=T,f=100", 10, 10), at(110, 0), GEO);
    images.kitty("Ga=d", at(100, 0), GEO);

    let rows: Vec<i64> = images.all().iter().map(|p| p.row).collect();
    assert_eq!(rows, [50], "the one in the scrollback stays");
}

#[test]
fn kitty_delete_leaves_other_protocols_alone() {
    let mut images = Images::new();
    images.iterm(
        &format!("File=inline=1:{}", STANDARD.encode(png(10, 10))),
        at(100, 0),
        GEO,
    );
    images.kitty("Ga=d,d=A", at(100, 0), GEO);
    assert_eq!(images.visible(100, 24).len(), 1);
}

#[test]
fn unsupported_kitty_features_say_so() {
    let mut images = Images::new();
    let unicode = images.kitty(&kitty_png("a=T,f=100,i=1,U=1", 10, 10), at(100, 0), GEO);
    assert!(reply_text(&unicode).contains("EINVAL"));
    let shm = images.kitty("Ga=T,t=s,f=100,i=2;bmFtZQ==", at(100, 0), GEO);
    assert!(reply_text(&shm).contains("shared memory"), "{}", reply_text(&shm));
    assert!(images.visible(100, 24).is_empty());
}

#[test]
fn kitty_file_and_temporary_file_transmission() {
    let dir = std::env::temp_dir();
    let file = dir.join(format!("unterm-images-test-{}.png", std::process::id()));
    std::fs::write(&file, png(10, 10)).unwrap();
    let path = STANDARD.encode(file.to_string_lossy().as_bytes());
    let mut images = Images::new();
    let outcome = images.kitty(&format!("Ga=T,t=f,f=100,i=1;{path}"), at(100, 0), GEO);
    assert!(reply_text(&outcome).ends_with(";OK\x1b\\"), "{}", reply_text(&outcome));
    assert!(file.exists(), "t=f must not delete the file");

    let temp = dir.join(format!("tty-graphics-protocol-{}.png", std::process::id()));
    std::fs::write(&temp, png(10, 10)).unwrap();
    let path = STANDARD.encode(temp.to_string_lossy().as_bytes());
    let outcome = images.kitty(&format!("Ga=T,t=t,f=100,i=2;{path}"), at(100, 0), GEO);
    assert!(reply_text(&outcome).ends_with(";OK\x1b\\"), "{}", reply_text(&outcome));
    assert!(!temp.exists(), "t=t deletes the temporary file");
    std::fs::remove_file(&file).ok();
}

#[test]
#[cfg(unix)]
fn a_device_is_not_read_as_a_picture() {
    let mut images = Images::new();
    let path = STANDARD.encode("/dev/zero");
    let outcome = images.kitty(&format!("Ga=T,t=f,f=100,i=1;{path}"), at(100, 0), GEO);
    assert!(reply_text(&outcome).contains("not a regular file"), "{}", reply_text(&outcome));
}

// ----------------------------------------------------------------- sixel

#[test]
fn a_sixel_image_is_placed_and_moves_the_cursor_down() {
    let mut images = Images::new();
    // 1 pixel wide, 42 tall: seven bands.
    let body = "q#1;2;100;0;0#1~-~-~-~-~-~-~";
    let outcome = images.sixel(body, at(100, 3), GEO);
    let shown = &images.visible(100, 24)[0];
    assert_eq!((shown.cols, shown.rows), (1, 3));
    assert_eq!(outcome.cursor, Some(CursorMove { down: 2, col: 3 }));
}

// ------------------------------------------------------------------ rows

fn one_at(images: &mut Images, row: i64) {
    images.kitty(&kitty_png("a=T,f=100", 10, 40), at(row, 0), GEO);
}

#[test]
fn trimming_history_moves_pictures_up_and_drops_the_ones_gone() {
    let mut images = Images::new();
    one_at(&mut images, 3); // rows 3..5
    one_at(&mut images, 10);
    images.history_trimmed(4);

    let rows: Vec<i64> = images.all().iter().map(|p| p.row).collect();
    // The first still has a row left (row -1 to 0): kept, partly cut off.
    assert_eq!(rows, [-1, 6]);
    images.history_trimmed(1);
    let rows: Vec<i64> = images.all().iter().map(|p| p.row).collect();
    assert_eq!(rows, [5]);
}

#[test]
fn a_region_scroll_moves_pictures_inside_it_only() {
    let mut images = Images::new();
    one_at(&mut images, 102);
    one_at(&mut images, 110);
    one_at(&mut images, 120);
    // Rows 105..=115 scroll up by 3: the one at 110 moves to 107.
    images.region_scrolled(105, 115, -3);
    let rows: Vec<i64> = images.all().iter().map(|p| p.row).collect();
    assert_eq!(rows, [102, 107, 120]);
    // Scrolled out of the top of the region, it is gone.
    images.region_scrolled(105, 115, -5);
    let rows: Vec<i64> = images.all().iter().map(|p| p.row).collect();
    assert_eq!(rows, [102, 120]);
}

#[test]
fn clearing_rows_removes_the_pictures_on_them() {
    let mut images = Images::new();
    one_at(&mut images, 100); // 100..101
    one_at(&mut images, 110);
    images.rows_cleared(101, 101);
    let rows: Vec<i64> = images.all().iter().map(|p| p.row).collect();
    assert_eq!(rows, [110]);
}

#[test]
fn the_alternate_screen_has_its_own_pictures() {
    let mut images = Images::new();
    one_at(&mut images, 100);
    images.enter_alternate();
    assert!(images.visible(100, 24).is_empty());
    one_at(&mut images, 101);
    assert_eq!(images.visible(100, 24).len(), 1);
    images.leave_alternate();
    let rows: Vec<i64> = images.visible(100, 24).iter().map(|p| p.row).collect();
    assert_eq!(rows, [100]);
}

#[test]
fn visible_includes_a_picture_that_starts_above_the_viewport() {
    let mut images = Images::new();
    one_at(&mut images, 98); // 98..99
    assert_eq!(images.visible(99, 24).len(), 1);
    assert!(images.visible(100, 24).is_empty());
}

#[test]
fn the_same_picture_is_stored_once() {
    let mut images = Images::new();
    one_at(&mut images, 100);
    one_at(&mut images, 105);
    let shown = images.visible(100, 24);
    assert_eq!(shown[0].image, shown[1].image);
    assert_eq!(images.store.len(), 1);
}

#[test]
fn image_data_and_placements_travel_as_json() {
    let mut images = Images::new();
    one_at(&mut images, 100);
    let placement = images.visible(100, 24).remove(0);
    let data = images.image_data(&placement.image).unwrap();

    let json = serde_json::to_string(&data).unwrap();
    let back: ImageData = serde_json::from_str(&json).unwrap();
    assert_eq!(back, data);

    let json = serde_json::to_string(&placement).unwrap();
    assert!(json.contains("\"protocol\":\"kitty\""), "{json}");
    let back: ImagePlacementSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(back, placement);
}
