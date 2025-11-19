use insta::*;
use m3u8_rs::*;
use streamrr::record::*;
use url::Url;

#[test]
fn test_rewrite_mux_llhls() {
    let url = Url::parse("https://manifest-gcp-us-east1-vop1.fastly.mux.com/4bTH98fZX5ztAEmXYDvBswYld6T9JifyucrlKYlz8AFLGaUvcHw2r56o302rPymJ5v4AkPRnEwr011eMWnyYWThjc4LUho3Zig/rendition.m3u8").unwrap();
    let mut playlist = parse_media_playlist_res(include_bytes!("fixtures/mux_llhls.m3u8")).unwrap();
    rewrite_media_playlist(&url, &mut playlist, &mut None).unwrap();
    assert_snapshot!(media_playlist_to_string(&playlist));
}

#[test]
fn test_rewrite_elephants_dream() {
    let url = Url::parse("https://cdn.theoplayer.com/video/elephants-dream/1280/chunklist_w370587926_b2962000_vo_slen_t64TWFpbg==.m3u8").unwrap();
    let mut playlist =
        parse_media_playlist_res(include_bytes!("fixtures/elephants_dream.m3u8")).unwrap();
    rewrite_media_playlist(&url, &mut playlist, &mut None).unwrap();
    assert_snapshot!(media_playlist_to_string(&playlist));
}

fn media_playlist_to_string(playlist: &MediaPlaylist) -> String {
    let mut buffer = vec![];
    playlist.write_to(&mut buffer).unwrap();
    String::from_utf8(buffer).unwrap()
}
