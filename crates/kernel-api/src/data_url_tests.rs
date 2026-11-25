use super::*;

#[test]
fn a_base64_data_url_is_written_and_read_back() {
    let text = base64("image/png", "iVBORw0K");
    assert_eq!(text, "data:image/png;base64,iVBORw0K");
    assert_eq!(
        parse(&text),
        Some(DataUrl {
            media_type: "image/png",
            data: "iVBORw0K"
        })
    );
}

/// 只认 base64 的写法，数据不能为空（Node `isUsableDataUrl`）。
#[test]
fn other_texts_are_not_data_urls() {
    for bad in [
        "data:image/png;base64,",
        "data:text/plain,hello",
        "image/png;base64,iVBORw0K",
        "zcode-artifact://ses_1/tool-result-1",
    ] {
        assert_eq!(parse(bad), None, "{bad}");
    }
}
