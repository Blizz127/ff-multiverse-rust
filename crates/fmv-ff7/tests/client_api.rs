//! Compile-time pin of the items `fmv-client` (`backdrop.rs`) and the server consume. If one of
//! these signatures or public fields changes, this file stops compiling instead of the client.
//!
//! No game data is involved; nothing here runs a decoder.

use fmv_adapter_api::AdapterError;
use fmv_ff7::background::{decode_background, BackgroundImage, Projection, ScreenPoint, ScreenRect};
use fmv_ff7::field::{Camera, FieldError, FieldFile};
use fmv_ff7::{Ff7Adapter, Ff7FieldArea};

#[test]
fn client_facing_signatures_are_stable() {
    // fmv_ff7::background
    let _: fn(&FieldFile) -> Result<BackgroundImage, FieldError> = decode_background;
    let _: fn(&Camera) -> Projection = Projection::from_camera;
    let _: fn(&Projection, [f32; 3]) -> Option<ScreenPoint> = Projection::project;
    let _: fn(&BackgroundImage) -> ScreenRect = BackgroundImage::bounds;
    let _: fn(&BackgroundImage) -> Vec<u8> = BackgroundImage::flatten_on_black;
    // fmv_ff7 area API
    let _: fn(&Ff7FieldArea) -> Result<Vec<Camera>, FieldError> = Ff7FieldArea::cameras;
    let _: for<'a> fn(&'a Ff7FieldArea) -> &'a FieldFile = Ff7FieldArea::field_file;
    let _: fn(&Ff7Adapter, &str) -> Result<Ff7FieldArea, AdapterError> = Ff7Adapter::load_field;

    // public fields the client reads
    let rect = ScreenRect { min_x: -1, min_y: -2, max_x: 3, max_y: 4 };
    let ScreenRect { min_x, min_y, max_x, max_y } = rect;
    assert_eq!((min_x, min_y, max_x, max_y), (-1, -2, 3, 4));
    let point = ScreenPoint { x: 1.0, y: 2.0, depth: 3.0 };
    let (x, y): (f32, f32) = (point.x, point.y);
    assert_eq!((x, y), (1.0, 2.0));
    let read_image = |image: &BackgroundImage| -> (u32, u32, usize) { (image.width, image.height, image.rgba.len()) };
    let _ = read_image;
}
