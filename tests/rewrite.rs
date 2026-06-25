use insta::*;
use m3u8_rs::*;
use streamrr::record::*;
use url::Url;

#[test]
fn test_rewrite_mux_llhls() {
    let url = Url::parse("https://manifest-gcp-us-east1-vop1.fastly.mux.com/4bTH98fZX5ztAEmXYDvBswYld6T9JifyucrlKYlz8AFLGaUvcHw2r56o302rPymJ5v4AkPRnEwr011eMWnyYWThjc4LUho3Zig/rendition.m3u8").unwrap();
    let mut playlist = parse_media_playlist_res(include_bytes!("fixtures/mux_llhls.m3u8")).unwrap();
    let mut rewriter = Rewriter::new(&url, "mux_llhls".as_ref(), false);
    rewriter.rewrite_media_playlist(&mut playlist).unwrap();
    assert_snapshot!(media_playlist_to_string(&playlist));
}

#[test]
fn test_rewrite_elephants_dream_master() {
    let url = Url::parse("https://cdn.theoplayer.com/video/elephants-dream/playlist.m3u8").unwrap();
    let mut playlist =
        parse_master_playlist_res(include_bytes!("fixtures/elephants_dream_master.m3u8")).unwrap();
    let rewriter = Rewriter::new(&url, "elephants_dream".as_ref(), false);
    rewriter.rewrite_master_playlist(&mut playlist).unwrap();
    assert_snapshot!(master_playlist_to_string(&playlist));
}

#[test]
fn test_rewrite_elephants_dream() {
    let url = Url::parse("https://cdn.theoplayer.com/video/elephants-dream/1280/chunklist_w370587926_b2962000_vo_slen_t64TWFpbg==.m3u8").unwrap();
    let mut playlist =
        parse_media_playlist_res(include_bytes!("fixtures/elephants_dream.m3u8")).unwrap();
    let mut rewriter = Rewriter::new(&url, "elephants_dream".as_ref(), false);
    rewriter.rewrite_media_playlist(&mut playlist).unwrap();
    assert_snapshot!(media_playlist_to_string(&playlist));
}

fn master_playlist_to_string(playlist: &MasterPlaylist) -> String {
    let mut buffer = vec![];
    playlist.write_to(&mut buffer).unwrap();
    String::from_utf8(buffer).unwrap()
}

fn media_playlist_to_string(playlist: &MediaPlaylist) -> String {
    let mut buffer = vec![];
    playlist.write_to(&mut buffer).unwrap();
    String::from_utf8(buffer).unwrap()
}
