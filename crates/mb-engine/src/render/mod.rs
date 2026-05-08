// Two-stage rendering pipeline:
//   1. `display_list` walks the LayoutBox tree and emits `DisplayCommand`s
//      (positioning, opacity composition, transform baking).
//   2. `raster` consumes the command list and paints pixels into a `Vec<u32>`.
//
// mod.rs holds the shared data types and the public re-exports; the algorithm
// halves live in sibling submodules so each stage can grow without crowding
// the other.

use crate::{css::{Color, GradientKind}, layout::Rect};

mod display_list;
mod raster;

pub use display_list::{PaintContext, build_display_list, transform_for, translate};
pub use raster::{measure_text_width, measure_text_wrap, measure_text_wrap_with_family, rasterize};

/// 2-D affine transform stored as the six matrix entries of
/// ```text
/// | a c e |
/// | b d f |
/// | 0 0 1 |
/// ```
/// `apply_point` premultiplies a column vector `(x, y, 1)`. `compose` matches
/// CSS semantics: `parent.compose(child).apply_point(p)` is the same as
/// `parent.apply_point(child.apply_point(p))`, so transforms inherit naturally
/// down the tree just like `inherited_alpha`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

impl Affine {
    pub const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    pub fn translate(tx: f32, ty: f32) -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: tx,
            f: ty,
        }
    }

    pub fn scale(sx: f32, sy: f32) -> Self {
        Self {
            a: sx,
            b: 0.0,
            c: 0.0,
            d: sy,
            e: 0.0,
            f: 0.0,
        }
    }

    /// `theta` is in radians (CSS deg/rad/turn/grad are normalised at parse time).
    pub fn rotate(theta: f32) -> Self {
        let (s, c) = theta.sin_cos();
        Self {
            a: c,
            b: s,
            c: -s,
            d: c,
            e: 0.0,
            f: 0.0,
        }
    }

    /// True iff the matrix is a pure translate+scale (no rotation/shear).
    /// The fast paint+raster path relies on this — when it's false, the
    /// box's primitives have to flow through the slow inverse-pixel-sample
    /// path inside `TransformGroup`.
    pub fn is_axis_aligned(&self) -> bool {
        self.b == 0.0 && self.c == 0.0
    }

    /// Standard 3x3 matrix multiply, restricted to the affine submatrix.
    pub fn compose(&self, other: Self) -> Self {
        Self {
            a: self.a * other.a + self.c * other.b,
            b: self.b * other.a + self.d * other.b,
            c: self.a * other.c + self.c * other.d,
            d: self.b * other.c + self.d * other.d,
            e: self.a * other.e + self.c * other.f + self.e,
            f: self.b * other.e + self.d * other.f + self.f,
        }
    }

    pub fn apply_point(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    /// Strict equality is fine because every non-identity matrix in the system
    /// is built from explicit constructors — no floating-point drift sneaks in
    /// when the page declares no transform at all.
    pub fn is_identity(&self) -> bool {
        self.a == 1.0
            && self.b == 0.0
            && self.c == 0.0
            && self.d == 1.0
            && self.e == 0.0
            && self.f == 0.0
    }

    /// Returns the matrix that undoes this one, or identity if the linear part
    /// is degenerate (zero determinant). The hit-test path uses this to map a
    /// screen-space cursor back into the layout-tree's logical coordinates so
    /// it can compare against the un-transformed `padding_box` rectangles.
    pub fn inverse(&self) -> Self {
        let det = self.a * self.d - self.b * self.c;
        if det == 0.0 {
            return Self::IDENTITY;
        }
        let inv_det = 1.0 / det;
        let a = self.d * inv_det;
        let b = -self.b * inv_det;
        let c = -self.c * inv_det;
        let d = self.a * inv_det;
        let e = -(a * self.e + c * self.f);
        let f = -(b * self.e + d * self.f);
        Self { a, b, c, d, e, f }
    }
}


#[derive(Debug, Clone, PartialEq)]
pub enum DisplayCommand {
    SolidRect(Color, Rect),
    RoundedRect(Color, Rect, CornerRadii),
    Text(TextCommand),
    Image(ImageCommand),
    /// Linear or radial gradient fill. Stops are pre-resolved to absolute
    /// positions in 0..1 along the gradient axis so the rasterizer doesn't
    /// have to redo the auto-position math.
    Gradient(GradientCommand),
    /// Outset box-shadow: a colored rectangle (already shifted by offset and
    /// inflated by spread) with a linear-ramp blur band around the edges.
    BoxShadow(ShadowCommand),
    /// A flat list of primitive commands rendered through the given affine
    /// matrix. The paint pass wraps a box's emitted primitives in this when
    /// the inherited+own matrix has rotation (b != 0 || c != 0), so that
    /// the rasterizer can scan-convert through the matrix instead of trying
    /// to bake rotation into axis-aligned rect coordinates. Translate+scale
    /// matrices skip the wrapper and bake into the rect directly to keep
    /// the fast rasterizer path on the common case.
    TransformGroup(Affine, Vec<DisplayCommand>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShadowCommand {
    pub rect: Rect,
    pub blur_radius: f32,
    pub color: Color,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GradientCommand {
    pub rect: Rect,
    pub kind: GradientKind,
    pub stops: Vec<ResolvedStop>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedStop {
    pub position: f32,
    pub color: Color,
}

// Per-corner radii so tabs (top corners only) and pills (uniform) share one primitive.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CornerRadii {
    pub tl: f32,
    pub tr: f32,
    pub br: f32,
    pub bl: f32,
}

impl CornerRadii {
    pub fn uniform(radius: f32) -> Self {
        Self {
            tl: radius,
            tr: radius,
            br: radius,
            bl: radius,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextCommand {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub color: Color,
    pub font_size: f32,
    // Mid-line wrap budget. `Some(w)` asks the rasteriser to break the
    // shaped run at `w` pixels (so a long paragraph paints as multiple
    // lines stacked below `y`); `None` shapes the whole string as a single
    // unwrapped line — the right shape for chrome chrome / single-line
    // input values where the caller has already handled line splitting.
    pub wrap_width: Option<f32>,
    // Cascaded `font-family` value, lowercased. Only the generic keyword
    // "monospace" is acted on today (routes shaping through cosmic-text's
    // `Family::Monospace`); every other value is treated as the default
    // sans-serif, matching what the renderer would do without a family at
    // all. `None` means the cascade had no `font-family` for this run.
    pub font_family: Option<String>,
    // Cascaded `font-weight` resolved to the CSS numeric scale (1-1000).
    // 400 is the default (`normal`), 700 maps to `bold`. Values ≥ 600 ask
    // the rasteriser to pick a bold face from cosmic-text's matched
    // family. Only the keywords `normal`/`bold` and explicit numeric
    // values land here; relative `bolder`/`lighter` resolve to the parent
    // weight at cascade time.
    pub font_weight: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImageCommand {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub source_width: usize,
    pub source_height: usize,
    pub pixels: Vec<u32>,
}

#[cfg(test)]
mod tests {
    use crate::{css, html, layout, render, style};

    use super::{
        Color, CornerRadii, DisplayCommand, ImageCommand, TextCommand, rasterize, translate,
    };

    fn display_list(html_source: &str, css_source: &str) -> Vec<DisplayCommand> {
        let document = html::parse(html_source).unwrap();
        let root = document.roots()[0];
        let stylesheet = css::parse(css_source).unwrap();
        let styled = style::style_tree(&document, root, &[stylesheet]);
        let layout = layout::layout_tree(&styled, 400.0);
        let images = std::collections::HashMap::new();
        render::build_display_list(&layout, &render::PaintContext::empty(&images))
    }

    #[test]
    fn paints_background_image_from_url() {
        // `background-image: url(...)` resolves the raw CSS string against
        // the document's base URL and looks up an entry in the images map
        // keyed by the resolved URL. The emitted Image command is anchored
        // to the padding box (matching the bg-color path) and stretches the
        // source pixels to fill it. Layout pads aren't required to make the
        // assertion meaningful here — a 50×30 box keeps the math obvious.
        let document = html::parse(r#"<div id="card"></div>"#).unwrap();
        let root = document.roots()[0];
        let stylesheet = css::parse(
            r#"
                #card {
                    width: 50px;
                    height: 30px;
                    background-image: url("/bg.png");
                }
            "#,
        )
        .unwrap();
        let styled = style::style_tree(&document, root, &[stylesheet]);
        let layout = layout::layout_tree(&styled, 400.0);

        let base = crate::url::Url::parse("http://example.com/page").unwrap();
        let resolved = base.resolve("/bg.png").unwrap();
        let mut images: std::collections::HashMap<String, crate::resource::LoadedImage> =
            std::collections::HashMap::new();
        images.insert(
            resolved.to_string(),
            crate::resource::LoadedImage {
                url: resolved,
                width: 2,
                height: 2,
                pixels: vec![0xFF0000, 0x00FF00, 0x0000FF, 0xFFFFFF],
            },
        );
        let ctx = render::PaintContext {
            base_url: Some(&base),
            images: &images,
        };
        let commands = render::build_display_list(&layout, &ctx);

        let image = commands
            .iter()
            .find_map(|cmd| match cmd {
                DisplayCommand::Image(image) => Some(image),
                _ => None,
            })
            .expect("background-image url(...) must emit an Image command");
        assert_eq!(image.x, 0.0);
        assert_eq!(image.y, 0.0);
        assert_eq!(image.width, 50.0);
        assert_eq!(image.height, 30.0);
        assert_eq!(image.source_width, 2);
        assert_eq!(image.source_height, 2);
    }

    #[test]
    fn paints_background_rect_from_padding_box() {
        let commands = display_list(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    width: 100px;
                    height: 40px;
                    padding-left: 5px;
                    padding-right: 7px;
                    padding-top: 3px;
                    padding-bottom: 9px;
                    background-color: #336699;
                }
            "#,
        );

        assert_eq!(
            commands,
            vec![DisplayCommand::SolidRect(
                Color {
                    r: 51,
                    g: 102,
                    b: 153,
                    a: 255,
                },
                crate::layout::Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 112.0,
                    height: 52.0,
                }
            )]
        );
    }

    #[test]
    fn paints_text_nodes_with_inherited_style() {
        let commands = display_list(
            r#"<p class="copy">Hello</p>"#,
            r#"
                .copy {
                    color: #0f0;
                    font-size: 18px;
                }
            "#,
        );

        assert_eq!(
            commands,
            vec![DisplayCommand::Text(TextCommand {
                text: "Hello".into(),
                x: 0.0,
                // Paint y = content_y (12, from <p> UA margin-top) + half-leading.
                // With Phase 6.B normal line-height = 1.2× the line box is 21.6
                // for an 18px font; half-leading = (21.6 - 18) / 2 = 1.8 → y = 13.8.
                y: 13.8,
                color: Color {
                    r: 0,
                    g: 255,
                    b: 0,
                    a: 255,
                },
                font_size: 18.0,
                // Paragraph text emits with the layout box's content width as
                // its wrap budget. "Hello" measures 67.5px under the toy
                // estimate (5 chars * 18 * 0.75) and never reaches the 400px
                // viewport cap, so the box settles at the shrink-to-fit
                // measured width and that becomes the wrap budget too.
                wrap_width: Some(67.5),
                font_family: None,
                font_weight: 400,
            })]
        );
    }

    #[test]
    fn code_inline_emits_text_with_monospace_family_keyword() {
        // Phase 5.5: a bare `<code>` arrives at the renderer with the UA
        // default `font-family: monospace`, which inherits down to the
        // text leaf. The text-leaf LayoutBox is what emits the
        // TextCommand, so the `font_family` field is what proves the
        // cascade reached the rasteriser — without it, raster's
        // `attrs_for_run` falls back to sans-serif and the glyph row
        // shapes with the wrong font even when Menlo is loaded.
        let commands = display_list(r#"<code>x</code>"#, "");
        let text = commands
            .iter()
            .find_map(|cmd| match cmd {
                DisplayCommand::Text(text) if text.text == "x" => Some(text),
                _ => None,
            })
            .expect("code element must paint its text leaf");
        assert_eq!(text.font_family.as_deref(), Some("monospace"));
    }

    #[test]
    fn bold_inline_emits_text_with_700_font_weight() {
        // Phase 6.C mirror of the monospace test: a UA-bold `<b>` run
        // must arrive at the rasteriser with `font_weight = 700` so
        // cosmic-text picks the bold face from the matched family. A
        // sibling plain-text run must stay at the 400 default to prove
        // inheritance only flows through the bolded subtree.
        let commands = display_list(r#"<p>plain<b>bold</b></p>"#, "");
        let mut bold_weight = None;
        let mut plain_weight = None;
        for cmd in &commands {
            if let DisplayCommand::Text(text) = cmd {
                match text.text.as_str() {
                    "bold" => bold_weight = Some(text.font_weight),
                    "plain" => plain_weight = Some(text.font_weight),
                    _ => {}
                }
            }
        }
        assert_eq!(bold_weight, Some(700));
        assert_eq!(plain_weight, Some(400));
    }

    #[test]
    fn inline_code_paints_author_background_and_padding() {
        // Phase 5.5: an author-styled inline `<code>` (background + padding)
        // must emit a SolidRect at its padding-box. The inline whitelist
        // (layout::inline) already gives `<code>` its own LayoutBox with
        // padding folded into the box dims, so this test is what catches
        // any future regression where inline elements stop running through
        // the paint_self/background_command path. The padding-box width is
        // measured("x") + 4px*2 ≈ 16.5px under the toy text estimate.
        let commands = display_list(
            r#"<p><code>x</code></p>"#,
            r#"
                code {
                    background-color: #ff0000;
                    padding-left: 4px;
                    padding-right: 4px;
                    padding-top: 2px;
                    padding-bottom: 2px;
                }
            "#,
        );

        let red_rect = commands
            .iter()
            .find_map(|cmd| match cmd {
                DisplayCommand::SolidRect(color, rect)
                    if *color
                        == (Color {
                            r: 255,
                            g: 0,
                            b: 0,
                            a: 255,
                        }) =>
                {
                    Some(rect)
                }
                _ => None,
            })
            .expect("inline <code>'s author background must paint as a SolidRect");
        // Padding-box height = content_height + top + bottom. Default font-size is
        // 16, content_height = 16 × 1.2 = 19.2 (Phase 6.B normal line-height),
        // plus 2+2 padding gives 23.2.
        assert_eq!(red_rect.height, 23.2);
        // Padding-box width = inline content width + 4 + 4 = at least 8px wider than 0.
        assert!(
            red_rect.width >= 8.0,
            "padding-box width must include 4px left+right padding (got {})",
            red_rect.width,
        );
    }

    #[test]
    fn hn_votearrow_inline_block_paints_sprite_via_background_shorthand() {
        // Phase 5.6: end-to-end probe of the HN-style sprite. An
        // inline-block sized purely by author CSS, with the sprite
        // installed via the `background:` shorthand, must paint as an
        // Image command at the box's padding box. This catches future
        // regressions in any of three pieces — shorthand parsing,
        // image lookup, or padding-box geometry — under a single
        // assertion. Inline-block already takes width/height correctly
        // and the renderer already resolves `background-image`; the
        // pre-5.6 gap was that the shorthand silently dropped on the
        // trailing `no-repeat` token, so neither side ever saw the URL.
        let document = html::parse(r##"<a class="vote"></a>"##).unwrap();
        let root = document.roots()[0];
        let stylesheet = css::parse(
            r#"
                .vote {
                    display: inline-block;
                    width: 10px;
                    height: 10px;
                    background: url("/grayarrow.gif") no-repeat;
                }
            "#,
        )
        .unwrap();
        let styled = style::style_tree(&document, root, &[stylesheet]);
        let layout = layout::layout_tree(&styled, 400.0);

        let base = crate::url::Url::parse("http://example.com/").unwrap();
        let resolved = base.resolve("/grayarrow.gif").unwrap();
        let mut images: std::collections::HashMap<String, crate::resource::LoadedImage> =
            std::collections::HashMap::new();
        images.insert(
            resolved.to_string(),
            crate::resource::LoadedImage {
                url: resolved,
                width: 1,
                height: 1,
                pixels: vec![0x808080],
            },
        );
        let ctx = render::PaintContext {
            base_url: Some(&base),
            images: &images,
        };
        let commands = render::build_display_list(&layout, &ctx);

        let sprite = commands
            .iter()
            .find_map(|cmd| match cmd {
                DisplayCommand::Image(img) => Some(img),
                _ => None,
            })
            .expect("sprite must paint as an Image command");
        assert_eq!(sprite.width, 10.0);
        assert_eq!(sprite.height, 10.0);
    }

    #[test]
    fn pre_paints_ua_default_background_panel() {
        // Phase 5.5: even with no author CSS, a `<pre>` should arrive at
        // the painter as a panel — the faint #f6f8fa background is what
        // separates a code block from surrounding prose. This test pins
        // the UA color so a future cascade tweak that drops the bg shows
        // up here instead of silently regressing the page's look.
        let commands = display_list(r#"<pre>x</pre>"#, "");
        let bg = commands
            .iter()
            .find_map(|cmd| match cmd {
                DisplayCommand::SolidRect(color, rect)
                    if *color
                        == (Color {
                            r: 246,
                            g: 248,
                            b: 250,
                            a: 255,
                        }) =>
                {
                    Some(rect)
                }
                _ => None,
            })
            .expect("<pre> UA-default background must paint as a SolidRect");
        // 8px top + 8px bottom padding wraps a one-line glyph row, so the
        // padding-box is taller than the glyph itself (>= 16 + 16).
        assert!(
            bg.height >= 32.0,
            "padding-box height must include 8px top+bottom padding (got {})",
            bg.height,
        );
    }

    #[test]
    fn input_emits_background_border_and_value_text() {
        // Step 6.1: an unstyled <input value="hi"> renders as background +
        // four border edges + a Text command for the value attribute. UA
        // defaults give it 200×16 content, 1px borders, 4×2 padding, so
        // the value text sits at the content origin (5, 3).
        let commands = display_list(r#"<input type="text" value="hi"/>"#, "");

        let solid_rects: Vec<_> = commands
            .iter()
            .filter(|cmd| matches!(cmd, DisplayCommand::SolidRect(_, _)))
            .collect();
        // Expect: 1 white background + 4 border edges (top/bottom/left/right).
        assert_eq!(solid_rects.len(), 5);

        // The value attribute drives a Text command. We don't pin the exact
        // coordinates because the underlying intrinsic-height math may
        // legitimately drift; what matters is that the *value string* is
        // what gets painted (not, e.g., the placeholder, not nothing).
        let value_text = commands
            .iter()
            .find_map(|cmd| match cmd {
                DisplayCommand::Text(text) if text.text == "hi" => Some(text),
                _ => None,
            })
            .expect("input must paint its value attribute as a Text command");
        assert_eq!(value_text.font_size, 16.0);
    }

    #[test]
    fn input_with_empty_value_skips_text_command() {
        // No value attribute → no Text command emitted (no placeholder
        // rendering yet). The bg + border still paint so the field is
        // visible as a clickable target.
        let commands = display_list(r#"<input type="text"/>"#, "");

        assert!(
            commands
                .iter()
                .all(|cmd| !matches!(cmd, DisplayCommand::Text(_))),
            "empty <input> should not emit any Text commands"
        );
        let solid_count = commands
            .iter()
            .filter(|cmd| matches!(cmd, DisplayCommand::SolidRect(_, _)))
            .count();
        assert_eq!(solid_count, 5, "bg + 4 borders still paint");
    }

    #[test]
    fn textarea_emits_one_text_command_per_newline_delimited_line() {
        // <textarea value="a\nb\nc"> should emit three Text commands —
        // one per line — stacked top-to-bottom from the content origin.
        // The y deltas equal the cascaded font-size (16px UA default),
        // and each command's text is the line in source order.
        let commands = display_list(
            "<textarea value=\"a\nb\nc\"></textarea>",
            "",
        );

        let texts: Vec<_> = commands
            .iter()
            .filter_map(|cmd| match cmd {
                DisplayCommand::Text(text) => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(texts.len(), 3, "three lines should produce three Text commands");
        assert_eq!(texts[0].text, "a");
        assert_eq!(texts[1].text, "b");
        assert_eq!(texts[2].text, "c");
        // Lines stack vertically by font-size; x stays pinned to the
        // content origin since soft wrapping is not implemented.
        assert_eq!(texts[1].y - texts[0].y, 16.0);
        assert_eq!(texts[2].y - texts[1].y, 16.0);
        assert_eq!(texts[0].x, texts[1].x);
        assert_eq!(texts[0].x, texts[2].x);
    }

    #[test]
    fn textarea_skips_empty_lines_in_paint_but_keeps_following_lines_offset() {
        // A blank middle line (`a\n\nb`) emits no Text command for the
        // empty entry — there's nothing to paint — but the following
        // line still sits two rows below the first because the caret
        // path counts blank rows separately.
        let commands = display_list(
            "<textarea value=\"a\n\nb\"></textarea>",
            "",
        );

        let texts: Vec<_> = commands
            .iter()
            .filter_map(|cmd| match cmd {
                DisplayCommand::Text(text) => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(texts.len(), 2);
        assert_eq!(texts[0].text, "a");
        assert_eq!(texts[1].text, "b");
        // Two-row gap between the two visible lines (the empty middle
        // line still consumes a row even though it didn't paint).
        assert_eq!(texts[1].y - texts[0].y, 32.0);
    }

    #[test]
    fn paints_rect_before_descendant_text() {
        let commands = display_list(
            r#"<div id="card"><p>Hello</p></div>"#,
            r#"
                #card {
                    background-color: #111111;
                }

                p {
                    font-size: 20px;
                }
            "#,
        );

        assert!(matches!(commands[0], DisplayCommand::SolidRect(_, _)));
        assert!(matches!(commands[1], DisplayCommand::Text(_)));
    }

    #[test]
    fn rasterizes_background_pixels() {
        let pixels = rasterize(
            &[DisplayCommand::SolidRect(
                Color {
                    r: 255,
                    g: 0,
                    b: 0,
                    a: 255,
                },
                crate::layout::Rect {
                    x: 1.0,
                    y: 1.0,
                    width: 2.0,
                    height: 2.0,
                },
            )],
            4,
            4,
        );

        assert_eq!(pixels[5], 0xFF0000);
        assert_eq!(pixels[10], 0xFF0000);
        assert_eq!(pixels[0], 0xFFFFFF);
    }

    #[test]
    fn translates_display_commands() {
        let commands = translate(
            vec![
                DisplayCommand::SolidRect(
                    Color::BLACK,
                    crate::layout::Rect {
                        x: 1.0,
                        y: 2.0,
                        width: 3.0,
                        height: 4.0,
                    },
                ),
                DisplayCommand::Text(TextCommand {
                    text: "hello".into(),
                    x: 5.0,
                    y: 6.0,
                    color: Color::BLACK,
                    font_size: 8.0,
                    wrap_width: None,
                    font_family: None,
                    font_weight: 400,
                }),
                DisplayCommand::Image(ImageCommand {
                    x: 7.0,
                    y: 8.0,
                    width: 9.0,
                    height: 10.0,
                    source_width: 1,
                    source_height: 1,
                    pixels: vec![0x112233],
                }),
            ],
            10.0,
            20.0,
        );

        assert_eq!(
            commands[0],
            DisplayCommand::SolidRect(
                Color::BLACK,
                crate::layout::Rect {
                    x: 11.0,
                    y: 22.0,
                    width: 3.0,
                    height: 4.0,
                },
            )
        );
        assert_eq!(
            commands[1],
            DisplayCommand::Text(TextCommand {
                text: "hello".into(),
                x: 15.0,
                y: 26.0,
                color: Color::BLACK,
                font_size: 8.0,
                wrap_width: None,
                font_family: None,
                font_weight: 400,
            })
        );
        assert_eq!(
            commands[2],
            DisplayCommand::Image(ImageCommand {
                x: 17.0,
                y: 28.0,
                width: 9.0,
                height: 10.0,
                source_width: 1,
                source_height: 1,
                pixels: vec![0x112233],
            })
        );
    }

    #[test]
    fn rasterizes_image_pixels() {
        let pixels = rasterize(
            &[DisplayCommand::Image(ImageCommand {
                x: 0.0,
                y: 0.0,
                width: 2.0,
                height: 2.0,
                source_width: 2,
                source_height: 2,
                pixels: vec![0xFF0000, 0x00FF00, 0x0000FF, 0xFFFFFF],
            })],
            2,
            2,
        );

        assert_eq!(pixels, vec![0xFF0000, 0x00FF00, 0x0000FF, 0xFFFFFF]);
    }

    #[test]
    fn paints_borders_when_color_and_width_are_present() {
        let commands = display_list(
            r#"<div class="panel"></div>"#,
            r#"
                .panel {
                    width: 20px;
                    height: 10px;
                    border-left: 2px;
                    border-right: 2px;
                    border-top: 1px;
                    border-bottom: 3px;
                    border-color: #112233;
                }
            "#,
        );

        assert_eq!(
            commands,
            vec![
                DisplayCommand::SolidRect(
                    Color {
                        r: 17,
                        g: 34,
                        b: 51,
                        a: 255,
                    },
                    crate::layout::Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 24.0,
                        height: 1.0,
                    },
                ),
                DisplayCommand::SolidRect(
                    Color {
                        r: 17,
                        g: 34,
                        b: 51,
                        a: 255,
                    },
                    crate::layout::Rect {
                        x: 0.0,
                        y: 11.0,
                        width: 24.0,
                        height: 3.0,
                    },
                ),
                DisplayCommand::SolidRect(
                    Color {
                        r: 17,
                        g: 34,
                        b: 51,
                        a: 255,
                    },
                    crate::layout::Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 2.0,
                        height: 14.0,
                    },
                ),
                DisplayCommand::SolidRect(
                    Color {
                        r: 17,
                        g: 34,
                        b: 51,
                        a: 255,
                    },
                    crate::layout::Rect {
                        x: 22.0,
                        y: 0.0,
                        width: 2.0,
                        height: 14.0,
                    },
                ),
            ]
        );
    }

    #[test]
    fn css_border_radius_emits_rounded_rect_background() {
        let commands = display_list(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    width: 100px;
                    height: 40px;
                    background-color: #336699;
                    border-radius: 8px;
                }
            "#,
        );

        // First command is the background; non-zero border-radius selects RoundedRect.
        match &commands[0] {
            DisplayCommand::RoundedRect(_, _, radii) => {
                assert_eq!(radii.tl, 8.0);
                assert_eq!(radii.tr, 8.0);
                assert_eq!(radii.br, 8.0);
                assert_eq!(radii.bl, 8.0);
            }
            other => panic!("expected RoundedRect background, got {other:?}"),
        }
    }

    #[test]
    fn css_border_radius_four_value_shorthand_assigns_each_corner() {
        let commands = display_list(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    width: 100px;
                    height: 40px;
                    background-color: #336699;
                    border-radius: 1px 2px 3px 4px;
                }
            "#,
        );

        // 4-value shorthand maps to tl/tr/br/bl in source order.
        match &commands[0] {
            DisplayCommand::RoundedRect(_, _, radii) => {
                assert_eq!(radii.tl, 1.0);
                assert_eq!(radii.tr, 2.0);
                assert_eq!(radii.br, 3.0);
                assert_eq!(radii.bl, 4.0);
            }
            other => panic!("expected RoundedRect background, got {other:?}"),
        }
    }

    #[test]
    fn rounded_rect_with_zero_radius_matches_solid_rect() {
        let rect = crate::layout::Rect {
            x: 0.0,
            y: 0.0,
            width: 4.0,
            height: 4.0,
        };
        let color = Color {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };

        let solid = rasterize(&[DisplayCommand::SolidRect(color, rect)], 4, 4);
        let rounded = rasterize(
            &[DisplayCommand::RoundedRect(
                color,
                rect,
                CornerRadii::default(),
            )],
            4,
            4,
        );

        assert_eq!(solid, rounded);
    }

    #[test]
    fn rounded_rect_uniform_radius_clips_all_four_corners() {
        let pixels = rasterize(
            &[DisplayCommand::RoundedRect(
                Color {
                    r: 255,
                    g: 0,
                    b: 0,
                    a: 255,
                },
                crate::layout::Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 4.0,
                    height: 4.0,
                },
                CornerRadii::uniform(2.0),
            )],
            4,
            4,
        );

        // All four corner pixels lie outside the inscribed circle and stay white.
        assert_eq!(pixels[0], 0xFFFFFF, "(0,0) clipped by tl");
        assert_eq!(pixels[3], 0xFFFFFF, "(3,0) clipped by tr");
        assert_eq!(pixels[12], 0xFFFFFF, "(0,3) clipped by bl");
        assert_eq!(pixels[15], 0xFFFFFF, "(3,3) clipped by br");

        // Pixels just inside each corner stay filled.
        assert_eq!(pixels[5], 0xFF0000, "(1,1) inside tl arc");
        assert_eq!(pixels[10], 0xFF0000, "(2,2) inside br arc");
    }

    #[test]
    fn rounded_rect_per_corner_radii_only_clip_specified_corner() {
        let pixels = rasterize(
            &[DisplayCommand::RoundedRect(
                Color {
                    r: 255,
                    g: 0,
                    b: 0,
                    a: 255,
                },
                crate::layout::Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 4.0,
                    height: 4.0,
                },
                CornerRadii {
                    tl: 2.0,
                    tr: 0.0,
                    br: 0.0,
                    bl: 0.0,
                },
            )],
            4,
            4,
        );

        // Only the top-left corner is rounded; the other three corners stay sharp.
        assert_eq!(pixels[0], 0xFFFFFF, "(0,0) clipped by tl");
        assert_eq!(pixels[3], 0xFF0000, "(3,0) tr is sharp");
        assert_eq!(pixels[12], 0xFF0000, "(0,3) bl is sharp");
        assert_eq!(pixels[15], 0xFF0000, "(3,3) br is sharp");
    }

    #[test]
    fn translate_moves_rounded_rect_position_only() {
        let commands = translate(
            vec![DisplayCommand::RoundedRect(
                Color::BLACK,
                crate::layout::Rect {
                    x: 1.0,
                    y: 2.0,
                    width: 10.0,
                    height: 6.0,
                },
                CornerRadii::uniform(3.0),
            )],
            4.0,
            5.0,
        );

        assert_eq!(
            commands[0],
            DisplayCommand::RoundedRect(
                Color::BLACK,
                crate::layout::Rect {
                    x: 5.0,
                    y: 7.0,
                    width: 10.0,
                    height: 6.0,
                },
                CornerRadii::uniform(3.0),
            )
        );
    }

    fn solid_rect_colors(commands: &[DisplayCommand]) -> Vec<Color> {
        commands
            .iter()
            .filter_map(|cmd| match cmd {
                DisplayCommand::SolidRect(color, _) => Some(*color),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn positioned_box_paints_after_in_flow_sibling_even_when_earlier_in_dom() {
        // .abs is a position:absolute box that comes BEFORE .flow in DOM order.
        // Without stacking-context handling it would paint first and end up
        // covered by .flow. With the new pass it gets pushed to the positioned
        // layer and paints AFTER .flow.
        let commands = display_list(
            r#"<div id="root"><div class="abs"></div><div class="flow"></div></div>"#,
            r#"
                #root { width: 200px; height: 100px; background-color: #ffffff; }
                .abs {
                    position: absolute;
                    width: 50px;
                    height: 50px;
                    background-color: #ff0000;
                }
                .flow {
                    width: 200px;
                    height: 30px;
                    background-color: #0000ff;
                }
            "#,
        );

        let colors = solid_rect_colors(&commands);
        let red = Color {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        let blue = Color {
            r: 0,
            g: 0,
            b: 255,
            a: 255,
        };
        let red_idx = colors.iter().position(|c| *c == red).expect("red painted");
        let blue_idx = colors
            .iter()
            .position(|c| *c == blue)
            .expect("blue painted");

        assert!(
            blue_idx < red_idx,
            "in-flow blue ({blue_idx}) should paint before absolute red ({red_idx})"
        );
    }

    #[test]
    fn z_index_orders_positioned_siblings_ascending() {
        // Two absolutes; the one with z=1 should paint first, z=2 second.
        let commands = display_list(
            r#"<div id="root"><div class="back"></div><div class="front"></div></div>"#,
            r#"
                #root { width: 200px; height: 100px; }
                .back {
                    position: absolute;
                    z-index: 2;
                    width: 50px;
                    height: 50px;
                    background-color: #00ff00;
                }
                .front {
                    position: absolute;
                    z-index: 1;
                    width: 50px;
                    height: 50px;
                    background-color: #ff8800;
                }
            "#,
        );

        let colors = solid_rect_colors(&commands);
        let green = Color {
            r: 0,
            g: 255,
            b: 0,
            a: 255,
        };
        let orange = Color {
            r: 255,
            g: 136,
            b: 0,
            a: 255,
        };
        let green_idx = colors
            .iter()
            .position(|c| *c == green)
            .expect("green painted");
        let orange_idx = colors
            .iter()
            .position(|c| *c == orange)
            .expect("orange painted");

        // z=1 (orange) paints before z=2 (green). DOM order is reversed —
        // proves z-index drives ordering, not source order.
        assert!(
            orange_idx < green_idx,
            "z=1 ({orange_idx}) should paint before z=2 ({green_idx})"
        );
    }

    #[test]
    fn negative_z_index_paints_behind_in_flow_content() {
        // .behind has z=-1, so it sits underneath the in-flow .flow even
        // though the absolute would otherwise paint after.
        let commands = display_list(
            r#"<div id="root"><div class="behind"></div><div class="flow"></div></div>"#,
            r#"
                #root { width: 200px; height: 100px; }
                .behind {
                    position: absolute;
                    z-index: -1;
                    width: 50px;
                    height: 50px;
                    background-color: #ff0000;
                }
                .flow {
                    width: 200px;
                    height: 30px;
                    background-color: #0000ff;
                }
            "#,
        );

        let colors = solid_rect_colors(&commands);
        let red = Color {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        let blue = Color {
            r: 0,
            g: 0,
            b: 255,
            a: 255,
        };
        let red_idx = colors.iter().position(|c| *c == red).expect("red painted");
        let blue_idx = colors
            .iter()
            .position(|c| *c == blue)
            .expect("blue painted");

        assert!(
            red_idx < blue_idx,
            "z=-1 red ({red_idx}) should paint before in-flow blue ({blue_idx})"
        );
    }

    #[test]
    fn nested_positioned_descendants_belong_to_their_own_stacking_context() {
        // .outer is absolute. .inner (also absolute) is its child. From the
        // root's perspective .outer is one atomic z-layer — its children
        // should paint within that atom, NOT escape to the root's layer order.
        let commands = display_list(
            r#"<div id="root"><div class="flow"></div><div class="outer"><div class="inner"></div></div></div>"#,
            r#"
                #root { width: 200px; height: 100px; }
                .flow {
                    width: 200px;
                    height: 30px;
                    background-color: #888888;
                }
                .outer {
                    position: absolute;
                    width: 100px;
                    height: 60px;
                    background-color: #ff0000;
                }
                .inner {
                    position: absolute;
                    width: 30px;
                    height: 30px;
                    background-color: #00ff00;
                }
            "#,
        );

        let colors = solid_rect_colors(&commands);
        let gray = Color {
            r: 136,
            g: 136,
            b: 136,
            a: 255,
        };
        let red = Color {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        let green = Color {
            r: 0,
            g: 255,
            b: 0,
            a: 255,
        };
        let gray_idx = colors
            .iter()
            .position(|c| *c == gray)
            .expect("gray painted");
        let red_idx = colors.iter().position(|c| *c == red).expect("red painted");
        let green_idx = colors
            .iter()
            .position(|c| *c == green)
            .expect("green painted");

        // Order: in-flow gray, then absolute outer (red), then nested inner (green).
        assert!(gray_idx < red_idx, "in-flow before outer absolute");
        assert!(
            red_idx < green_idx,
            "outer paints before its inner descendant"
        );
    }

    #[test]
    fn text_glyph_is_offset_by_half_leading_inside_line_box() {
        // 40px line-height with 20px font-size leaves (40-20)/2 = 10px of
        // half-leading above the glyph. The text command y should land at
        // line_box_top + half_leading, which centers the glyph in the line.
        let commands = display_list(
            r#"<p>X</p>"#,
            r#"
                p {
                    font-size: 20px;
                    line-height: 40px;
                    margin-top: 0;
                    margin-bottom: 0;
                }
            "#,
        );

        let text = commands
            .iter()
            .find_map(|cmd| match cmd {
                DisplayCommand::Text(t) => Some(t),
                _ => None,
            })
            .expect("paragraph emits a Text command");

        // p has no margin/padding, so its content origin is (0, 0). The line
        // box top is at content_y = 0; glyph sits 10px below that.
        assert_eq!(text.y, 10.0);
        // font-size in the command stays 20 — line-height does not scale
        // glyph rendering, only the surrounding box.
        assert_eq!(text.font_size, 20.0);
    }

    fn first_solid_alpha_for(commands: &[DisplayCommand]) -> u8 {
        commands
            .iter()
            .find_map(|cmd| match cmd {
                DisplayCommand::SolidRect(color, _) => Some(color.a),
                _ => None,
            })
            .expect("at least one SolidRect")
    }

    #[test]
    fn opacity_attenuates_emitted_color_alpha() {
        // `opacity: 0.5` should multiply the background color's alpha by 0.5
        // when the SolidRect is emitted — 255 * 0.5 = 127.
        let commands = display_list(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    width: 50px;
                    height: 50px;
                    background-color: #ff0000;
                    opacity: 0.5;
                }
            "#,
        );

        assert_eq!(first_solid_alpha_for(&commands), 127);
    }

    #[test]
    fn nested_opacities_multiply() {
        // Inner `.b`'s alpha = parent 0.5 × own 0.5 = 0.25 → 255 × 0.25 = 63.
        let commands = display_list(
            r#"<div class="a"><div class="b"></div></div>"#,
            r#"
                .a {
                    width: 100px;
                    height: 100px;
                    background-color: #ff0000;
                    opacity: 0.5;
                }
                .b {
                    width: 50px;
                    height: 50px;
                    background-color: #0000ff;
                    opacity: 0.5;
                }
            "#,
        );

        let alphas: Vec<u8> = commands
            .iter()
            .filter_map(|cmd| match cmd {
                DisplayCommand::SolidRect(color, _) => Some(color.a),
                _ => None,
            })
            .collect();
        // Two rects: parent first (.a, alpha = 127), then child (.b, alpha = 63).
        assert_eq!(alphas[0], 127);
        assert_eq!(alphas[1], 63);
    }

    #[test]
    fn opacity_inherits_through_non_positioned_ancestor_into_positioned_descendant() {
        // The hard case: positioned descendants jump out of the normal paint
        // walk into a z-layer, so the alpha they inherit must come from the
        // collected ancestor chain — not from the stacking context root.
        // Final alpha for `.c` = 0.5 × 0.5 × 0.5 = 0.125 → 255 × 0.125 = 31.
        let commands = display_list(
            r#"<div class="a"><div class="b"><div class="c"></div></div></div>"#,
            r#"
                .a { width: 200px; height: 200px; opacity: 0.5; }
                .b { width: 100px; height: 100px; opacity: 0.5; }
                .c {
                    position: absolute;
                    width: 50px;
                    height: 50px;
                    background-color: #ff0000;
                    opacity: 0.5;
                }
            "#,
        );

        // Find the red rect — that's `.c`'s background.
        let red_alpha = commands
            .iter()
            .find_map(|cmd| match cmd {
                DisplayCommand::SolidRect(color, _) if color.r == 255 && color.g == 0 => {
                    Some(color.a)
                }
                _ => None,
            })
            .expect("red rect for .c");
        assert_eq!(red_alpha, 31);
    }

    #[test]
    fn linear_gradient_vertical_red_to_blue_interpolates_top_to_bottom() {
        // 1×4 strip with `linear-gradient(red, blue)` — top row should be
        // mostly red, bottom row mostly blue. Exact midpoints depend on
        // pixel-center sampling, so we just check the dominant channel.
        let commands = display_list(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    width: 1px;
                    height: 4px;
                    background-image: linear-gradient(red, blue);
                }
            "#,
        );
        let pixels = render::rasterize(&commands, 1, 4);

        let top_r = (pixels[0] >> 16) & 0xFF;
        let top_b = pixels[0] & 0xFF;
        let bottom_r = (pixels[3] >> 16) & 0xFF;
        let bottom_b = pixels[3] & 0xFF;

        assert!(top_r > 200, "top should be mostly red, got r={top_r}");
        assert!(top_b < 50, "top should have little blue, got b={top_b}");
        assert!(
            bottom_r < 50,
            "bottom should have little red, got r={bottom_r}"
        );
        assert!(
            bottom_b > 200,
            "bottom should be mostly blue, got b={bottom_b}"
        );
    }

    #[test]
    fn linear_gradient_to_right_interpolates_left_to_right() {
        // Same gradient, rotated to the horizontal axis — direction wins.
        let commands = display_list(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    width: 4px;
                    height: 1px;
                    background-image: linear-gradient(to right, red, blue);
                }
            "#,
        );
        let pixels = render::rasterize(&commands, 4, 1);

        let left_r = (pixels[0] >> 16) & 0xFF;
        let left_b = pixels[0] & 0xFF;
        let right_r = (pixels[3] >> 16) & 0xFF;
        let right_b = pixels[3] & 0xFF;

        assert!(left_r > 200, "left should be mostly red, got r={left_r}");
        assert!(
            right_b > 200,
            "right should be mostly blue, got b={right_b}"
        );
        assert!(left_b < 50);
        assert!(right_r < 50);
    }

    #[test]
    fn linear_gradient_explicit_stop_positions_pin_color_at_those_points() {
        // With `red 0%, blue 25%, blue 100%`, every pixel from x=1 onward in
        // a 4px wide row should already be pure blue (the second stop pins it).
        let commands = display_list(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    width: 4px;
                    height: 1px;
                    background-image: linear-gradient(to right, red 0%, blue 25%, blue 100%);
                }
            "#,
        );
        let pixels = render::rasterize(&commands, 4, 1);

        // Pixel index 1 sits at progress = 1.5/4 = 0.375 ≥ 0.25 → fully blue.
        assert_eq!(pixels[1], 0x000000FF);
        assert_eq!(pixels[2], 0x000000FF);
        assert_eq!(pixels[3], 0x000000FF);
    }

    #[test]
    fn text_shadow_emits_offset_text_command_before_main_text() {
        // `text-shadow: 2px 3px red` should produce two Text commands: the
        // shadow at (offset_x, offset_y) under the main glyph in red, and
        // the regular text on top with the inherited color.
        let commands = display_list(
            r#"<p>Hi</p>"#,
            r#"
                p {
                    font-size: 16px;
                    color: black;
                    text-shadow: 2px 3px red;
                    margin-top: 0;
                    margin-bottom: 0;
                }
            "#,
        );

        let texts: Vec<&TextCommand> = commands
            .iter()
            .filter_map(|cmd| match cmd {
                DisplayCommand::Text(text) => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(texts.len(), 2, "shadow + main = 2 text commands");

        let shadow = texts[0];
        let main = texts[1];

        // Shadow color = red.
        assert_eq!(shadow.color.r, 255);
        assert_eq!(shadow.color.g, 0);
        assert_eq!(shadow.color.b, 0);
        // Main color = black (inherited).
        assert_eq!(main.color.r, 0);
        assert_eq!(main.color.g, 0);
        assert_eq!(main.color.b, 0);

        // Shadow sits at +2,+3 relative to the main text.
        assert!((shadow.x - main.x - 2.0).abs() < f32::EPSILON);
        assert!((shadow.y - main.y - 3.0).abs() < f32::EPSILON);
        // Same glyph string and size.
        assert_eq!(shadow.text, main.text);
        assert_eq!(shadow.font_size, main.font_size);
    }

    #[test]
    fn text_without_text_shadow_emits_only_one_text_command() {
        // Sanity check: the shadow command shouldn't sneak in when no
        // text-shadow is declared on the element or any ancestor.
        let commands = display_list(
            r#"<p>Hi</p>"#,
            r#"
                p { font-size: 16px; }
            "#,
        );
        let text_count = commands
            .iter()
            .filter(|cmd| matches!(cmd, DisplayCommand::Text(_)))
            .count();
        assert_eq!(text_count, 1);
    }

    #[test]
    fn box_shadow_offset_paints_solid_outside_box_with_no_blur() {
        // 2×2 box at (0, 0) with `box-shadow: 2px 2px 0 0 red`. Shadow lands
        // at (2, 2)–(4, 4) with no blur, so pixels there should be solid red
        // and pixels inside the box itself stay covered by its own bg.
        let commands = display_list(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    width: 2px;
                    height: 2px;
                    background-color: white;
                    box-shadow: 2px 2px 0 0 red;
                }
            "#,
        );
        let pixels = render::rasterize(&commands, 4, 4);

        // (3, 3): inside the shadow, fully red.
        assert_eq!(pixels[3 * 4 + 3], 0x00FF0000);
        // (0, 0): inside the box's own white background — shadow not visible there.
        assert_eq!(pixels[0], 0x00FFFFFF);
    }

    #[test]
    fn box_shadow_blur_softens_alpha_outside_rect() {
        // `box-shadow: 0 0 4px black` with no offset. Inside the rect the
        // box's own white bg paints over the shadow, so the test focuses on
        // pixels OUTSIDE: ones close to the edge get a soft darken from the
        // linear-ramp blur, ones beyond the blur radius are untouched.
        let commands = display_list(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    width: 4px;
                    height: 4px;
                    background-color: white;
                    box-shadow: 0 0 4px black;
                }
            "#,
        );
        // 12×12 buffer leaves at least a 4px margin on every side of the box.
        let pixels = render::rasterize(&commands, 12, 12);

        // (5, 2): 1.5px past the right edge → coverage 1 - 1.5/4 = 0.625,
        // pixel reads roughly mid-gray.
        let near = pixels[2 * 12 + 5];
        let near_r = (near >> 16) & 0xFF;
        assert!(
            near_r < 200,
            "near-edge pixel should be visibly darkened, got r={near_r}"
        );

        // (8, 2): 4.5px past the right edge — beyond the blur radius, so
        // coverage clamps to 0 and the pixel stays full white.
        let far = pixels[2 * 12 + 8];
        assert_eq!(
            far, 0x00FFFFFF,
            "pixel beyond the blur radius should be untouched"
        );
    }

    #[test]
    fn radial_gradient_centers_inner_color_with_outer_at_corners() {
        // 5×5 box with `radial-gradient(red, blue)` (ellipse, farthest-corner).
        // Center pixel should be the inner stop (red); corner pixels should
        // sample close to the outer stop (blue) since their normalised
        // distance from the centre approaches 1.
        let commands = display_list(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    width: 5px;
                    height: 5px;
                    background-image: radial-gradient(red, blue);
                }
            "#,
        );
        let pixels = render::rasterize(&commands, 5, 5);

        let center = pixels[2 * 5 + 2];
        let center_r = (center >> 16) & 0xFF;
        let center_b = center & 0xFF;
        assert!(
            center_r > 200,
            "center should be near red, got r={center_r}"
        );
        assert!(
            center_b < 50,
            "center should have little blue, got b={center_b}"
        );

        // Top-left corner: distance ≈ sqrt(2)/2·diag → progress ≈ 1 → blue.
        let corner = pixels[0];
        let corner_r = (corner >> 16) & 0xFF;
        let corner_b = corner & 0xFF;
        assert!(
            corner_b > 200,
            "corner should be near blue, got b={corner_b}"
        );
        assert!(
            corner_r < 50,
            "corner should have little red, got r={corner_r}"
        );
    }

    #[test]
    fn fill_rect_alpha_blends_with_existing_pixel() {
        // White buffer + 50% red → red channel stays 255, green/blue mix to ~127.
        let red_half = Color {
            r: 255,
            g: 0,
            b: 0,
            a: 128,
        };
        let pixels = render::rasterize(
            &[DisplayCommand::SolidRect(
                red_half,
                crate::layout::Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                },
            )],
            1,
            1,
        );

        // Expected: r ≈ 255, g and b ≈ 127. Single u32 = 0xFF7F7F.
        assert_eq!(pixels[0], 0x00FF7F7F);
    }

    #[test]
    fn transform_translate_shifts_emitted_solid_rect() {
        // `transform: translate(5px, 10px)` should leave the box's logical
        // size alone but move the painted rect by (5, 10).
        let commands = display_list(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    width: 20px;
                    height: 8px;
                    background-color: red;
                    transform: translate(5px, 10px);
                }
            "#,
        );

        let rect = match commands.as_slice() {
            [DisplayCommand::SolidRect(_, rect)] => *rect,
            other => panic!("expected one SolidRect, got {other:?}"),
        };
        // Logical box was at (0, 0) with width 20, height 8. The translate
        // shifts the origin only — width and height stay invariant for now.
        assert_eq!(rect.x, 5.0);
        assert_eq!(rect.y, 10.0);
        assert_eq!(rect.width, 20.0);
        assert_eq!(rect.height, 8.0);
    }

    #[test]
    fn transform_translate_inherits_to_descendant() {
        // The parent's translate should compose into the child's emitted
        // commands as well (paint-pass thread of `inherited_transform`).
        let commands = display_list(
            r#"<div id="outer"><div id="inner"></div></div>"#,
            r#"
                #outer { transform: translate(50px, 0); }
                #inner {
                    width: 10px;
                    height: 4px;
                    background-color: blue;
                }
            "#,
        );

        let inner_rect = commands
            .iter()
            .find_map(|cmd| match cmd {
                DisplayCommand::SolidRect(_, rect) if rect.width == 10.0 => Some(*rect),
                _ => None,
            })
            .expect("inner rect must be emitted");
        assert_eq!(inner_rect.x, 50.0);
        assert_eq!(inner_rect.y, 0.0);
    }

    #[test]
    fn transform_scale_grows_box_around_its_center() {
        // `scale(2)` doubles the rect dimensions and (because the default
        // origin is the box centre) the new origin is shifted by half the
        // growth along each axis. A 20x10 rect at (0, 0) → 40x20 at (-10, -5).
        let commands = display_list(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    width: 20px;
                    height: 10px;
                    background-color: red;
                    transform: scale(2);
                }
            "#,
        );

        let rect = match commands.as_slice() {
            [DisplayCommand::SolidRect(_, rect)] => *rect,
            other => panic!("expected one SolidRect, got {other:?}"),
        };
        assert!((rect.width - 40.0).abs() < 1e-4);
        assert!((rect.height - 20.0).abs() < 1e-4);
        assert!((rect.x - -10.0).abs() < 1e-4);
        assert!((rect.y - -5.0).abs() < 1e-4);
    }

    #[test]
    fn transform_scale_combines_with_translate_in_source_order() {
        // `transform: translate(100px, 0) scale(2)` reads left-to-right as
        // "scale around the box centre, then translate". Composition follows
        // source order, so the post-scale rect is shifted by (100, 0).
        let commands = display_list(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    width: 10px;
                    height: 4px;
                    background-color: blue;
                    transform: translate(100px, 0) scale(2);
                }
            "#,
        );

        let rect = match commands.as_slice() {
            [DisplayCommand::SolidRect(_, rect)] => *rect,
            other => panic!("expected one SolidRect, got {other:?}"),
        };
        // Original 10x4 at (0,0). scale(2) around center → 20x8 at (-5,-2).
        // translate(100,0) → 20x8 at (95,-2).
        assert!((rect.width - 20.0).abs() < 1e-4);
        assert!((rect.height - 8.0).abs() < 1e-4);
        assert!((rect.x - 95.0).abs() < 1e-4);
        assert!((rect.y - -2.0).abs() < 1e-4);
    }

    #[test]
    fn affine_inverse_undoes_scale_and_compose() {
        // Round-trip a non-trivial translate+scale: after applying T then T^-1
        // the point should be unchanged. This is the operation behind hit-test
        // for scaled boxes.
        let t = super::Affine::translate(50.0, 10.0).compose(super::Affine::scale(2.0, 4.0));
        let (x, y) = t.compose(t.inverse()).apply_point(7.0, 3.0);
        assert!((x - 7.0).abs() < 1e-4);
        assert!((y - 3.0).abs() < 1e-4);
    }

    #[test]
    fn transform_rotate_wraps_emitted_commands_in_transform_group() {
        // Rotation breaks axis-aligned baking, so apply_transform must
        // route the box's primitives through a TransformGroup with the
        // cumulative matrix attached. The inner SolidRect should still be
        // in the box's logical coordinates.
        let commands = display_list(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    width: 20px;
                    height: 10px;
                    background-color: red;
                    transform: rotate(45deg);
                }
            "#,
        );

        match commands.as_slice() {
            [DisplayCommand::TransformGroup(transform, inner)] => {
                assert!(
                    !transform.is_axis_aligned(),
                    "rotate must produce a non-axis-aligned matrix"
                );
                match inner.as_slice() {
                    [DisplayCommand::SolidRect(_, rect)] => {
                        // Logical rect — pre-transform — at the box's
                        // own (0, 0) with the declared 20x10 size.
                        assert_eq!(rect.x, 0.0);
                        assert_eq!(rect.y, 0.0);
                        assert_eq!(rect.width, 20.0);
                        assert_eq!(rect.height, 10.0);
                    }
                    other => panic!("expected one SolidRect inside group, got {other:?}"),
                }
            }
            other => panic!("expected one TransformGroup, got {other:?}"),
        }
    }

    #[test]
    fn transform_rotate_paints_pixel_at_post_rotation_position() {
        // 90deg rotates the box's right edge to the bottom. Centre of the
        // 10x4 logical box maps to roughly the same screen point (since
        // we rotate around the centre by default), so the centre pixel
        // should still be filled — but a corner pixel that was inside
        // the unrotated box must now miss.
        let commands = display_list(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    width: 10px;
                    height: 4px;
                    background-color: red;
                    transform: rotate(90deg);
                }
            "#,
        );
        // Larger canvas so the rotated quad (which now extends to ~10px
        // tall and ~4px wide centred on (5, 2)) lands cleanly in-frame.
        let pixels = render::rasterize(&commands, 16, 16);
        // Logical centre is (5, 2); rotation around that point keeps the
        // centre pixel painted. We pick (5, 2) on the screen and assert
        // it's red.
        let centre = pixels[2 * 16 + 5];
        assert_eq!(centre & 0x00FFFFFF, 0x00FF0000);

        // Pre-rotation, screen pixel (8, 2) sat inside the 10x4 box
        // (close to its right edge). After rotating 90° around (5, 2)
        // that screen position now maps to logical (5, -1) — outside
        // the box — so it should NOT be painted red.
        let outside = pixels[2 * 16 + 8];
        assert_ne!(outside & 0x00FFFFFF, 0x00FF0000);
    }

    #[test]
    fn affine_rotate_round_trips_a_point_through_inverse() {
        let theta = std::f32::consts::FRAC_PI_3; // 60°
        let t = super::Affine::rotate(theta);
        let (x, y) = t.compose(t.inverse()).apply_point(11.0, -4.0);
        assert!((x - 11.0).abs() < 1e-4);
        assert!((y + 4.0).abs() < 1e-4);
    }

    #[test]
    fn affine_inverse_undoes_translate() {
        // Round-trip: applying a translate then its inverse to a point must
        // return the original point. This is the operation hit-test relies on.
        let t = super::Affine::translate(7.0, -3.5);
        let (x, y) = t.compose(t.inverse()).apply_point(11.0, 22.0);
        assert!((x - 11.0).abs() < 1e-5);
        assert!((y - 22.0).abs() < 1e-5);

        // The inverse of a translate is just the negation of the offsets.
        let inv = t.inverse();
        let (x, y) = inv.apply_point(10.0, 10.0);
        assert!((x - 3.0).abs() < 1e-5);
        assert!((y - 13.5).abs() < 1e-5);
    }
}
