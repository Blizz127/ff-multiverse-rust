//! The client's own UI: text, panels, the pre-join character menu and the in-game HUD, drawn by the
//! wgpu renderer.
//!
//! No UI toolkit: the glyphs come from an embedded public-domain 8x8 bitmap font ([`font`]), and
//! everything is a list of textured rectangles ([`draw::DrawList`]) drawn in one extra pass over
//! the scene ([`gpu`]). The point of keeping it that small is that ALL the logic is plain data
//! and functions, testable without a window or a GPU:
//!
//! * [`font`], [`text`], [`draw`]: the font and its atlas, text measuring/wrapping/drawing
//!   (`DrawList::draw_text(x, y, scale, rgba, text)`), and the draw list itself.
//! * [`widgets`]: panels, buttons that show the keyboard focus, swatches, the colours of the UI.
//! * [`entry`], [`palette`]: editing the name, the tint palette.
//! * [`menu`]: the character select / create state machine. It takes key events and returns
//!   [`menu::MenuAction`]s; it does no I/O.
//! * [`screens`]: draws a [`menu::Menu`]; [`hud`] draws the in-game overlay; [`countdown`] keeps what the server last said
//!   about the countdown of the player's story (the first reactor's bomb) and draws it at the top centre of the HUD.
//! * [`input`]: the one place that turns winit key events into menu input.
//! * [`flow`], [`looks`], [`connect`], [`deleter`], [`shell`]: the impure edges, kept thin: turning a menu
//!   action into a login plan and a connection (or the notice a delete came to), the local cache of how
//!   saved characters look, the connection thread, the thread that deletes a character on the server, and
//!   the window handler that runs the menu and then the game.
//!
//! Tests: everything above has unit tests (`cargo test -p fmv-client`); the pictures are checked
//! offscreen with `cargo test -p fmv-client -- --ignored offscreen`, which also writes PNGs of
//! every screen and of the HUD to `~/.cache/claude-tmp/fmv-client-shots/`.

pub mod connect;
pub mod countdown;
pub mod deleter;
pub mod dialogue;
pub mod draw;
pub mod entry;
pub mod flow;
pub mod font;
pub mod gpu;
pub mod hud;
pub mod input;
pub mod looks;
pub mod menu;
pub mod palette;
pub mod screens;
pub mod shell;
pub mod text;
pub mod widgets;

#[cfg(test)]
mod shots;
#[cfg(test)]
pub(crate) mod testkit;
