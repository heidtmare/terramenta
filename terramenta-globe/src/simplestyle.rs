//! [simplestyle-spec 1.1.0]: the ten `properties` members a GeoJSON document
//! is allowed to style itself with.
//!
//! Everywhere else the globe treats `properties` as opaque — what a `mag` or a
//! `place` means is the feed's business, and [`crate::features`] holds the
//! whole object as the JSON text it arrived as. This module is the one
//! exception, and it is a narrow one: ten names, fixed by a specification,
//! whose whole purpose is to tell a renderer how the feature should look.
//!
//! **Only what a document states is honoured.** The specification lists a
//! default for every member — grey markers, a `#555555` stroke, a fill at 0.6
//! opacity — and none of those are applied here. A layer already has a colour,
//! chosen by whoever put the layer up, and a document that says nothing about
//! its appearance should come out in it rather than in the specification's
//! grey. So each member overrides exactly the one thing it names and nothing
//! else: `fill` replaces the layer's fill colour and leaves its opacity alone,
//! `fill-opacity` replaces the opacity and leaves the colour alone, and a
//! document with no style members at all is drawn exactly as it was before any
//! of this existed. That is also what makes the two compose — an interface can
//! recolour a layer and only the features that did not ask for a colour move.
//!
//! **What is drawn and what is handed on.** Five of the members are paint, and
//! the globe draws them: `marker-size`, `marker-color`, `stroke`,
//! `stroke-opacity`, `stroke-width`, `fill` and `fill-opacity`. Three are not
//! paint — `title`, `description` and `marker-symbol` — and the globe has no
//! label engine and no icon atlas to render them with. They are parsed and
//! carried anyway, and handed out with a picked feature, because an interface
//! that wants to put a title in a readout should not have to re-implement this
//! module to find one.
//!
//! **A member that cannot be read is ignored, never fatal.** A colour that is
//! not a colour, a width that is not a number, a `marker-size` that is not one
//! of the three: the member is dropped and the layer's own value stands. A
//! document should not lose its geometry over a typo in its styling.
//!
//! [simplestyle-spec 1.1.0]: https://github.com/mapbox/simplestyle-spec/tree/master/1.1.0

use bevy::color::{Alpha, Srgba};
use serde_json::{Map, Value};

use crate::overlays::{Paint, VectorMode};

/// What `marker-size` means in device pixels.
///
/// The specification says only small, medium and large, so the numbers are the
/// globe's to choose. Medium is the globe's own default marker diameter, which
/// is what makes a document that asks for medium markers come out looking like
/// a document that asked for nothing.
pub const MARKER_SMALL_PX: f32 = 6.0;
pub const MARKER_MEDIUM_PX: f32 = 9.0;
pub const MARKER_LARGE_PX: f32 = 15.0;

/// One of the three sizes `marker-size` allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerSize {
    Small,
    Medium,
    Large,
}

impl MarkerSize {
    /// The stable name the specification calls this by, which is also what an
    /// interface is handed back.
    pub fn id(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        match id {
            "small" => Some(Self::Small),
            "medium" => Some(Self::Medium),
            "large" => Some(Self::Large),
            _ => None,
        }
    }

    /// The marker diameter it asks for, on screen.
    pub fn diameter_px(self) -> f32 {
        match self {
            Self::Small => MARKER_SMALL_PX,
            Self::Medium => MARKER_MEDIUM_PX,
            Self::Large => MARKER_LARGE_PX,
        }
    }
}

/// The simplestyle members one feature carried.
///
/// Every field is optional and means "the document said so": `None` is not a
/// default, it is silence, and silence is what leaves the layer's own style
/// standing. See the module docs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SimpleStyle {
    /// `title` — a human-readable name for the feature.
    pub title: Option<String>,
    /// `description` — a longer note, which the specification allows to be
    /// HTML. Carried as it was written; rendering it is not the globe's job,
    /// and neither is trusting it.
    pub description: Option<String>,
    /// `marker-size`.
    pub marker_size: Option<MarkerSize>,
    /// `marker-symbol` — an icon id, a single character, or a Maki symbol
    /// name. Carried for an interface that has icons; the globe has none.
    pub marker_symbol: Option<String>,
    /// `marker-color`.
    pub marker_color: Option<Srgba>,
    /// `stroke`.
    pub stroke: Option<Srgba>,
    /// `stroke-opacity`, clamped to `[0, 1]`.
    pub stroke_opacity: Option<f32>,
    /// `stroke-width`, in device pixels, never negative.
    pub stroke_width: Option<f32>,
    /// `fill`.
    pub fill: Option<Srgba>,
    /// `fill-opacity`, clamped to `[0, 1]`.
    pub fill_opacity: Option<f32>,
}

impl SimpleStyle {
    /// Reads the style members out of a feature's `properties`.
    ///
    /// Everything that is not one of the ten names is left alone — it is the
    /// feed's data, and it goes on to an interface untouched.
    pub fn read(properties: &Map<String, Value>) -> Self {
        Self {
            title: properties.get("title").and_then(text),
            description: properties.get("description").and_then(text),
            marker_size: properties
                .get("marker-size")
                .and_then(text)
                .as_deref()
                .and_then(MarkerSize::from_id),
            marker_symbol: properties.get("marker-symbol").and_then(text),
            marker_color: properties.get("marker-color").and_then(color),
            stroke: properties.get("stroke").and_then(color),
            stroke_opacity: properties.get("stroke-opacity").and_then(opacity),
            // A width is a width: negative is not thinner, it is nothing, and
            // the specification says zero or more.
            stroke_width: properties
                .get("stroke-width")
                .and_then(number)
                .map(|width| width.max(0.0)),
            fill: properties.get("fill").and_then(color),
            fill_opacity: properties.get("fill-opacity").and_then(opacity),
        }
    }

    /// Whether the document said anything at all about this feature. A feature
    /// that said nothing is not stored, which is what keeps a feed of five
    /// thousand unstyled earthquakes from carrying five thousand empty structs.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Whether anything here changes how the feature is *drawn*, as opposed to
    /// what an interface is told about it. A feature carrying nothing but a
    /// `title` is styled as far as a readout is concerned and unstyled as far
    /// as the renderer is.
    pub fn paints(&self) -> bool {
        self.marker_size.is_some()
            || self.marker_color.is_some()
            || self.stroke.is_some()
            || self.stroke_opacity.is_some()
            || self.stroke_width.is_some()
            || self.fill.is_some()
            || self.fill_opacity.is_some()
    }

    /// What this feature's shapes of one kind are drawn with, over the layer's
    /// own paint for that kind.
    pub fn paint(&self, mode: VectorMode, base: Paint) -> Paint {
        match mode {
            VectorMode::Marker => self.marker(base),
            VectorMode::Line => self.stroke(base),
            VectorMode::Fill => self.fill(base),
        }
    }

    /// The layer's marker paint with `marker-color` and `marker-size` applied.
    ///
    /// `marker-color` carries no opacity of its own — there is no
    /// `marker-opacity` in the specification — so the layer's alpha stands,
    /// which is what lets an interface fade a whole layer without the styled
    /// markers staying solid.
    fn marker(&self, base: Paint) -> Paint {
        Paint {
            color: match self.marker_color {
                Some(color) => color.with_alpha(base.color.alpha),
                None => base.color,
            },
            size_px: self
                .marker_size
                .map_or(base.size_px, MarkerSize::diameter_px),
        }
    }

    /// The layer's line paint with `stroke`, `stroke-opacity` and
    /// `stroke-width` applied.
    fn stroke(&self, base: Paint) -> Paint {
        Paint {
            color: blend(base.color, self.stroke, self.stroke_opacity),
            size_px: self.stroke_width.unwrap_or(base.size_px),
        }
    }

    /// The layer's fill paint with `fill` and `fill-opacity` applied. A fill
    /// has no size; `base.size_px` is carried through untouched.
    fn fill(&self, base: Paint) -> Paint {
        Paint {
            color: blend(base.color, self.fill, self.fill_opacity),
            size_px: base.size_px,
        }
    }
}

/// A colour and an opacity over a base, each standing in only for itself.
fn blend(base: Srgba, color: Option<Srgba>, opacity: Option<f32>) -> Srgba {
    let color = color.map_or(base, |color| color.with_alpha(base.alpha));
    match opacity {
        Some(opacity) => color.with_alpha(opacity),
        None => color,
    }
}

/// A member that has to be text. A number written where a string belongs is
/// still what it says it is — a `title` of `2024` is a title.
fn text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// A member that has to be a number.
///
/// A numeric string is read as the number it spells, because feeds write
/// `"stroke-width": "2"` often enough that refusing it would only mean drawing
/// the wrong width. Anything that is not finite is not a number a renderer can
/// use.
fn number(value: &Value) -> Option<f32> {
    let number = match value {
        Value::Number(number) => number.as_f64()?,
        Value::String(text) => text.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    (number as f32).is_finite().then_some(number as f32)
}

/// An opacity: a number, clamped to the range the specification gives it.
fn opacity(value: &Value) -> Option<f32> {
    number(value).map(|opacity| opacity.clamp(0.0, 1.0))
}

/// A simplestyle colour: three or six hexadecimal digits, with or without a
/// leading `#`.
///
/// Both spellings, because the specification's own defaults are written each
/// way — `#555555` for the stroke and `7e7e7e` for the marker. Nothing else is
/// a colour here: an eight-digit value would be an alpha the specification has
/// separate members for, and a named colour is CSS rather than simplestyle.
fn color(value: &Value) -> Option<Srgba> {
    let text = match value {
        Value::String(text) => text,
        _ => return None,
    };
    let digits = text.trim().strip_prefix('#').unwrap_or(text.trim());
    if !digits.chars().all(|digit| digit.is_ascii_hexdigit()) {
        return None;
    }
    let pair = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).ok();
    let (red, green, blue) = match digits.len() {
        // `#ace` is `#aaccee`: each digit doubled, not shifted, so `f` is full
        // scale rather than fifteen sixteenths of it.
        3 => {
            let single = |at: usize| {
                u8::from_str_radix(&digits[at..at + 1], 16)
                    .ok()
                    .map(|value| value * 17)
            };
            (single(0)?, single(1)?, single(2)?)
        }
        6 => (pair(0)?, pair(2)?, pair(4)?),
        _ => return None,
    };
    Some(Srgba::rgb_u8(red, green, blue))
}

// ---------------------------------------------------------------------------
// What an interface is told
// ---------------------------------------------------------------------------

/// A feature's simplestyle, as it goes out with a pick.
///
/// The parsed members rather than the raw ones: an interface reading a title
/// out of `properties` would have to know that a number is allowed there too,
/// and one reading a colour would have to know that `#ace` is `#aaccee`. Both
/// of those are settled here, once.
#[derive(serde::Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SimpleStyleInfo {
    pub title: Option<String>,
    pub description: Option<String>,
    /// `"small"`, `"medium"` or `"large"`.
    pub marker_size: Option<&'static str>,
    pub marker_symbol: Option<String>,
    /// Hex, as a colour input takes it.
    pub marker_color: Option<String>,
    pub stroke: Option<String>,
    pub stroke_opacity: Option<f32>,
    pub stroke_width: Option<f32>,
    pub fill: Option<String>,
    pub fill_opacity: Option<f32>,
}

impl From<&SimpleStyle> for SimpleStyleInfo {
    fn from(style: &SimpleStyle) -> Self {
        // `to_hex` writes eight digits when the colour has an alpha; these
        // never do, because a simplestyle colour carries no alpha.
        let hex = |color: Option<Srgba>| color.map(|color| color.to_hex());
        Self {
            title: style.title.clone(),
            description: style.description.clone(),
            marker_size: style.marker_size.map(MarkerSize::id),
            marker_symbol: style.marker_symbol.clone(),
            marker_color: hex(style.marker_color),
            stroke: hex(style.stroke),
            stroke_opacity: style.stroke_opacity,
            stroke_width: style.stroke_width,
            fill: hex(style.fill),
            fill_opacity: style.fill_opacity,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(json: serde_json::Value) -> SimpleStyle {
        SimpleStyle::read(json.as_object().expect("an object"))
    }

    #[test]
    fn every_member_of_the_specification_is_read() {
        let read = style(serde_json::json!({
            "title": "Ridgecrest",
            "description": "<b>M 7.1</b>",
            "marker-size": "large",
            "marker-symbol": "circle",
            "marker-color": "#ff0000",
            "stroke": "#00ff00",
            "stroke-opacity": 0.5,
            "stroke-width": 4,
            "fill": "#0000ff",
            "fill-opacity": 0.25
        }));

        assert_eq!(read.title.as_deref(), Some("Ridgecrest"));
        assert_eq!(read.description.as_deref(), Some("<b>M 7.1</b>"));
        assert_eq!(read.marker_size, Some(MarkerSize::Large));
        assert_eq!(read.marker_symbol.as_deref(), Some("circle"));
        assert_eq!(read.marker_color, Some(Srgba::rgb_u8(255, 0, 0)));
        assert_eq!(read.stroke, Some(Srgba::rgb_u8(0, 255, 0)));
        assert_eq!(read.stroke_opacity, Some(0.5));
        assert_eq!(read.stroke_width, Some(4.0));
        assert_eq!(read.fill, Some(Srgba::rgb_u8(0, 0, 255)));
        assert_eq!(read.fill_opacity, Some(0.25));
        assert!(read.paints());
    }

    #[test]
    fn a_document_that_says_nothing_overrides_nothing() {
        let read = style(serde_json::json!({"mag": 4.2, "place": "somewhere"}));
        assert!(read.is_empty());
        assert!(!read.paints());

        let base = Paint {
            color: Srgba::new(1.0, 0.5, 0.0, 0.8),
            size_px: 9.0,
        };
        for mode in [VectorMode::Marker, VectorMode::Line, VectorMode::Fill] {
            assert_eq!(read.paint(mode, base), base);
        }
    }

    #[test]
    fn a_title_alone_is_not_paint() {
        // It still has to be carried — a readout wants it — but the mesh has
        // nothing to do with it, and should not grow a colour buffer over it.
        let read = style(serde_json::json!({"title": "Nowhere"}));
        assert!(!read.is_empty());
        assert!(!read.paints());
    }

    #[test]
    fn three_digits_mean_the_same_as_six() {
        assert_eq!(
            style(serde_json::json!({"fill": "#ace"})).fill,
            style(serde_json::json!({"fill": "#aaccee"})).fill
        );
        // Full scale rather than fifteen sixteenths: `#fff` is white.
        assert_eq!(
            style(serde_json::json!({"fill": "fff"})).fill,
            Some(Srgba::WHITE)
        );
    }

    #[test]
    fn a_colour_is_taken_with_or_without_a_hash() {
        // The specification's own defaults are written both ways.
        assert_eq!(
            style(serde_json::json!({"marker-color": "7e7e7e"})).marker_color,
            style(serde_json::json!({"marker-color": "#7e7e7e"})).marker_color
        );
    }

    #[test]
    fn a_member_that_cannot_be_read_is_simply_not_there() {
        let read = style(serde_json::json!({
            "marker-color": "rebeccapurple",
            "stroke": "#12345",
            "fill": "#11223344",
            "stroke-width": "wide",
            "marker-size": "enormous",
            "fill-opacity": true
        }));
        assert!(read.is_empty(), "{read:?}");
    }

    #[test]
    fn a_number_written_as_text_is_still_a_number() {
        let read = style(serde_json::json!({"stroke-width": "2.5", "fill-opacity": "0.5"}));
        assert_eq!(read.stroke_width, Some(2.5));
        assert_eq!(read.fill_opacity, Some(0.5));
    }

    #[test]
    fn an_opacity_is_clamped_and_a_width_is_never_negative() {
        let read = style(serde_json::json!({
            "stroke-opacity": 4.0,
            "fill-opacity": -1.0,
            "stroke-width": -3.0
        }));
        assert_eq!(read.stroke_opacity, Some(1.0));
        assert_eq!(read.fill_opacity, Some(0.0));
        assert_eq!(read.stroke_width, Some(0.0));
    }

    #[test]
    fn each_member_stands_in_only_for_itself() {
        let base = Paint {
            color: Srgba::new(1.0, 0.62, 0.24, 0.22),
            size_px: 2.0,
        };

        // A colour with no opacity keeps the layer's, so a feed that recolours
        // its rings does not also make them opaque.
        let recoloured = style(serde_json::json!({"fill": "#0000ff"}));
        let painted = recoloured.paint(VectorMode::Fill, base);
        assert_eq!(painted.color, Srgba::new(0.0, 0.0, 1.0, 0.22));

        // An opacity with no colour keeps the layer's colour.
        let faded = style(serde_json::json!({"fill-opacity": 0.9}));
        let painted = faded.paint(VectorMode::Fill, base);
        assert_eq!(painted.color, base.color.with_alpha(0.9));
    }

    #[test]
    fn a_marker_takes_its_size_from_the_three_names() {
        let base = Paint {
            color: Srgba::WHITE,
            size_px: 3.0,
        };
        let sized = |name: &str| {
            style(serde_json::json!({"marker-size": name}))
                .paint(VectorMode::Marker, base)
                .size_px
        };
        assert_eq!(sized("small"), MARKER_SMALL_PX);
        assert_eq!(sized("medium"), MARKER_MEDIUM_PX);
        assert_eq!(sized("large"), MARKER_LARGE_PX);
        // Marker colour is not stroke colour: a feature that set only `stroke`
        // leaves its markers as the layer drew them.
        let stroked = style(serde_json::json!({"stroke": "#ff0000"}));
        assert_eq!(stroked.paint(VectorMode::Marker, base), base);
    }

    #[test]
    fn a_stroke_width_is_a_width_and_not_a_marker_size() {
        let base = Paint {
            color: Srgba::WHITE,
            size_px: 2.0,
        };
        let read = style(serde_json::json!({"stroke-width": 6.0}));
        assert_eq!(read.paint(VectorMode::Line, base).size_px, 6.0);
        // A fill has no size of its own to take.
        assert_eq!(read.paint(VectorMode::Fill, base).size_px, base.size_px);
    }
}
