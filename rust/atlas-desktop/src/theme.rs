//! The palette, the type scale, the spacing scale and the frame's style — one place,
//! so no view has to invent a number and none can invent a colour.
//!
//! **Monochrome by construction, not by discipline.** Every colour is built with
//! [`Color32::from_gray`], which takes one luminance and gives back `r == g == b`, so a
//! hue cannot be typed here even by accident. That is not enough on its own: egui's
//! [`Visuals::dark`] fills the fields this module does not set with **blue, orange and
//! red** (`hyperlink_color` 90,170,255 · `warn_fg_color` 255,143,0 · `error_fg_color`
//! 255,0,0 · `selection` fill 0,92,128 · `text_cursor` 192,222,255 · the IME composition
//! underline 192,222,255 — see `egui-0.37.0/src/style.rs:1619-1755`), so "monochrome" is
//! only true once *every* colour field is assigned. [`styled`] assigns every one of
//! them, and `tests` walks the result instead of trusting this paragraph.
//!
//! egui 0.37 added places a colour can arrive from that 0.31 did not have —
//! `weak_text_color` (else `text_color × weak_text_alpha`, an unnamed semi-transparent
//! ink), `text_edit_bg_color` (else `extreme_bg_color`) and `ime_composition`'s two
//! underline strokes (blue by default). All three are assigned here as well; a version
//! bump is exactly when a "no hue survives" claim rots, so `tests` names those slots
//! explicitly rather than trusting a fresh `Visuals::dark()` to be grey.
//!
//! **One type scale.** Five named sizes ([`size`]) and two faces that actually ship
//! ([`Weight`]). Nothing here sets egui's `strong` flag: in egui 0.37 that flag is still
//! read only by `RichText::get_text_color` (`widget_text.rs:434`, `strong: _, // already
//! used by get_text_color`, resolved at `:504-505`), so a role that sets a colour and
//! then asks for "strong" renders *identically* to the same role without it — which is
//! why the old `card_title` and `body` were the same pixels at 13 px. Hierarchy comes
//! from size, colour and space instead of from a weight that does not exist. A genuinely
//! heavier face needs a `.ttf` in the tree; epaint ships Ubuntu-Light and Hack only, and
//! this crate may not download one, so [`Weight`] names the two real faces and `face()`
//! is checked against `FontDefinitions::builtin_font_names()` in `tests`.
//!
//! **Flat.** No shadow (`window_shadow`/`popup_shadow` are [`egui::epaint::Shadow::NONE`]),
//! no lift (`WidgetVisuals::expansion` is 0 in every state — egui's default grows a
//! widget by 1 px on hover and press), and no rounding above [`RADIUS`] px on any
//! corner egui can round (the defaults leave `menu_corner_radius` at 6 and
//! `hovered.corner_radius` at 3).
//!
//! **Focus is a decision, not a leftover.** egui has no focus visuals of its own: a
//! widget that has keyboard focus is styled with `WidgetVisuals::active`
//! (`egui-0.37.0/src/style.rs:1386`, `response.has_focus()` → `widgets.active`), so
//! that state is the focus ring. It is
//! [`FOCUS`] at [`FOCUS_WIDTH`] px — the brightest value in the palette, lighter than
//! body ink, so a focused control cannot be mistaken for ordinary text, and 19.80:1
//! against the page (WCAG asks 3:1 for a UI boundary; `tests` computes and asserts it).
//!
//! What this module does not decide: which face the operating system has beyond the two
//! bundled ones, and what a view *paints* itself with `Painter` (the map and the graph
//! build their own strokes; `theme::INK` / `theme::DIM` / `theme::FAINT` are the only
//! greys they need).

use egui::{
    style::{Selection, TextCursorStyle, WidgetVisuals, Widgets},
    vec2, Color32, CornerRadius, FontFamily, FontId, Margin, RichText, Stroke, Style, TextStyle,
    Visuals,
};

// ---------------------------------------------------------------------------- palette

/// The whole palette: one luminance per colour, so red, green and blue are the same
/// number by construction.
///
/// `ALL` is emitted by the same macro as the constants, so "walk every colour" cannot
/// miss one: adding a grey here and nowhere else is impossible.
macro_rules! palette {
    ($($(#[$meta:meta])* $name:ident = $grey:literal),* $(,)?) => {
        $($(#[$meta])* pub const $name: Color32 = Color32::from_gray($grey);)*

        /// Every colour this theme defines, with the name it is known by.
        pub const ALL: &[(&str, Color32)] = &[$( (stringify!($name), $name) ),*];
    };
}

palette! {
    /// The window behind everything.
    PAGE = 10,
    /// The deepest surface in the theme: the recess the entity graph is drawn on.
    ///
    /// A plate is a surface, not the page, and it is one step below [`PAGE`] because a field
    /// of dots and relation lines needs the darkest ground the interface has: everything
    /// drawn on it — ink, the two edge greys, the caption — is measured against *this*
    /// value, which is where the graph's own contrast claims are proved (`views::graph`,
    /// `the_weakest_relation_line_is_visible_on_the_plate`).
    RECESS = 7,
    /// A surface: a panel, an unfocused field, the recessed body of a card.
    PANEL = 16,
    /// A surface that is already raised: hover, press, a card on a panel.
    RAISED = 24,
    /// A structural line that is not meant to be read as a boundary.
    HAIRLINE_SOFT = 28,
    /// The one divider, border and separator.
    HAIRLINE = 36,
    /// A selected row's fill. Not a highlight colour: the row's ink does the talking.
    SELECT = 48,
    /// A faint boundary: the hover ring on a raised surface. 3.88:1 on the page and
    /// 3.48:1 on a raised one — a stroke, not ink; on a selected row it is only 2.59:1,
    /// so a boundary drawn on a selection uses [`DIM`].
    FAINT = 110,
    /// Secondary ink that is meant to be read as text: 7.12:1 on the page, 6.85:1 on a
    /// panel, 4.75:1 on a selected row.
    DIM = 155,
    /// Everything that is meant to be read. 17.68:1 on the page, 15.86:1 on a raised
    /// surface, 11.79:1 on a selected row.
    INK = 242,
    /// The keyboard focus ring, and the ink of a warning or an error: the brightest
    /// value in the palette, deliberately above [`INK`] so a state is never confused
    /// with ordinary text. 19.80:1 on the page, 13.20:1 on a selected row.
    FOCUS = 255,
}

/// The one stroke a card, a field or a divider uses.
pub fn line() -> Stroke {
    Stroke::new(HAIRLINE_WIDTH, HAIRLINE)
}

/// The keyboard focus ring. Painted by egui from `WidgetVisuals::active`, so this is
/// what every focused control wears.
pub fn focus_stroke() -> Stroke {
    Stroke::new(FOCUS_WIDTH, FOCUS)
}

/// A hairline that separates without a boundary: the rail's edge, a row's underline.
///
/// The two frames in `app.rs` build this stroke by hand
/// (`Stroke::new(1.0, theme::HAIRLINE_SOFT)`); they should call this instead, which is
/// why it exists here rather than being inlined (cross-file change, not made by this
/// pass).
#[allow(dead_code)]
pub fn soft_line() -> Stroke {
    Stroke::new(HAIRLINE_WIDTH, HAIRLINE_SOFT)
}

/// Stroke weights. Two: a hairline, and the focus ring.
pub const HAIRLINE_WIDTH: f32 = 1.0;
pub const FOCUS_WIDTH: f32 = 2.0;

/// The only corner radius in the interface. A 2 px corner reads as "made by a tool";
/// anything larger reads as a bubble, so egui's 3 px (hovered widgets) and 6 px (menus)
/// defaults are lowered to this.
pub const RADIUS: u8 = 2;

// ----------------------------------------------------------------------------- spacing

/// The spacing scale. One unit, and every gap, padding and margin is a multiple of it,
/// so two frames of the same view cannot be one pixel apart by accident.
pub mod space {
    /// The base unit. Everything below is a multiple of it.
    pub const UNIT: f32 = 4.0;
    /// Between a label and its value; between an icon and its text.
    pub const XS: f32 = 4.0;
    /// Between rows; the padding inside a control.
    pub const SM: f32 = 8.0;
    /// Between two cards; the top of a section.
    pub const MD: f32 = 12.0;
    /// Between a page's blocks; a card's inner margin.
    pub const LG: f32 = 16.0;
    /// Between the rail and the page; the height of a control.
    pub const XL: f32 = 24.0;
    /// The gap above a page title.
    pub const XXL: f32 = 32.0;
    /// A field, a slider and a combo box: 24 units wide, so a form column lines up.
    pub const COLUMN: f32 = 96.0;
}

/// Every spacing number [`styled`] installs, with the name it is known by. The test
/// asserts each is a positive multiple of [`space::UNIT`], and that the style carries
/// exactly these — so a number typed into the style cannot skip the scale.
pub const SPACING: &[(&str, f32)] = &[
    ("item spacing x", space::MD),
    ("item spacing y", space::SM),
    ("button padding x", space::SM),
    ("button padding y", space::XS),
    ("window margin", space::SM),
    ("menu margin", space::MD),
    ("indent", space::LG),
    ("interact width", space::XL),
    ("interact height", space::XL),
    ("icon width", space::LG),
    ("icon inner width", space::SM),
    ("icon spacing", space::XS),
    ("field width", space::COLUMN),
    ("scrollbar width", space::SM),
    ("scroll handle minimum", space::XL),
    ("scroll inner margin", space::XS),
    ("scroll outer margin", space::XS),
    ("clip rect margin", space::XS),
    ("resize corner", space::MD),
];

// --------------------------------------------------------------------------- type scale

/// The five sizes the interface uses, named by the job they do rather than by a number.
///
/// A ladder, not a pile: each step is 1.08–1.4 times the one below it (`tests` asserts
/// the ratios), so a reader sees a hierarchy instead of four similar sizes.
pub mod size {
    /// The smallest thing on the page: a timestamp under a headline, a count.
    pub const CAPTION: f32 = 11.0;
    /// Secondary text that is still prose: a hint, a note, a status line.
    pub const SMALL: f32 = 12.0;
    /// Running text: a headline in a list, a summary, a label, a button.
    pub const BODY: f32 = 13.0;
    /// A card's title, a number that matters, a section's name.
    pub const TITLE: f32 = 15.0;
    /// The page's own title. One per screen.
    pub const PAGE: f32 = 21.0;
}

/// The scale itself, smallest first.
pub const LADDER: [f32; 5] = [size::CAPTION, size::SMALL, size::BODY, size::TITLE, size::PAGE];

/// The proportional face epaint bundles. One weight, and this is it.
pub const TEXT_FACE: &str = "Ubuntu-Light";
/// The monospaced face epaint bundles. Heavier than [`TEXT_FACE`] at the same size,
/// which is why a number is set in it: a figure and a word are told apart by shape, not
/// by a fake bold.
pub const NUMBER_FACE: &str = "Hack";

/// A weight this interface can actually render.
///
/// Two, because two faces ship. There is no third: epaint has no synthetic
/// emboldening (a glyph is drawn with the face's own outlines — `TextFormat` can vary
/// letter spacing, italics and colour, and `coords` moves a *variable* font's axes, but
/// no variable face ships here), so "stronger" can only mean a different face or a
/// different colour. A role that wanted a third weight would need a `.ttf` in the tree
/// — see the module doc.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Weight {
    /// The text face: prose, labels, titles.
    Light,
    /// The numeric face: counts, weights, timestamps that must line up.
    Regular,
}

impl Weight {
    /// The family this weight resolves to.
    pub fn family(self) -> FontFamily {
        match self {
            Weight::Light => FontFamily::Proportional,
            Weight::Regular => FontFamily::Monospace,
        }
    }

    /// The face epaint loads for this family. Named so a test can prove the weight is
    /// real: this string has to be in `FontDefinitions::builtin_font_names()`.
    pub fn face(self) -> &'static str {
        match self {
            Weight::Light => TEXT_FACE,
            Weight::Regular => NUMBER_FACE,
        }
    }
}

/// One role in the type scale: a size, a weight, a colour. Text is built through
/// [`Role::text`] and nowhere else, so a view cannot pick a size that is not on the
/// ladder.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Role {
    /// How the role is named in a failure message.
    pub name: &'static str,
    pub size: f32,
    pub weight: Weight,
    pub colour: Color32,
}

impl Role {
    /// The font this role is set in: the size and the face, without the colour.
    ///
    /// Needed by any caller that must *measure* a line before it paints it — the status row
    /// reserves room for its right-hand label by laying that label out first. Measuring with
    /// the role's own font is what keeps the measurement and the paint in step: a number
    /// typed twice would drift the moment the ladder changed.
    pub fn font(&self) -> FontId {
        FontId::new(self.size, self.weight.family())
    }

    /// The role's text, ready for `ui.label`.
    pub fn text(&self, text: &str) -> RichText {
        RichText::new(text)
            .font(self.font())
            .color(self.colour)
    }
}

/// The seven roles. Five of them are what the views already rendered (`page_title`,
/// `card_title`, `body`, `dim`, `number`); `SECTION_LABEL` and `CAPTION` are the two the
/// interface was missing — a page of views had no smallest size, and no way to name a
/// group of rows except by writing a capitalised label by hand.
pub mod role {
    use super::{size, Role, Weight, DIM, INK};

    /// A page or dialog title. One per screen.
    pub const PAGE_TITLE: Role = Role {
        name: "page title",
        size: size::PAGE,
        weight: Weight::Light,
        colour: INK,
    };

    /// A card's title, or a dialog's section title.
    pub const CARD_TITLE: Role = Role {
        name: "card title",
        size: size::TITLE,
        weight: Weight::Light,
        colour: INK,
    };

    /// The name of a group of rows, one line above them: "Recent", "By country".
    ///
    /// **Sentence case, set by the caller and never by this module**: the theme does not
    /// change a string's case, so a label cannot become an eyebrow by accident. It is
    /// told apart from the rows it introduces by being one step larger than body text
    /// and by the space above it — not by capitals, not by letter-spacing, and not by a
    /// rule.
    pub const SECTION_LABEL: Role = Role {
        name: "section label",
        size: size::BODY,
        weight: Weight::Light,
        colour: INK,
    };

    /// Running text: a headline in a list, a summary, a button's words.
    pub const BODY: Role = Role {
        name: "body",
        size: size::BODY,
        weight: Weight::Light,
        colour: INK,
    };

    /// Secondary text that is still read: an age, a hint, a status.
    ///
    /// Named `SECONDARY` rather than `DIM` so it cannot be confused with the ink it uses;
    /// [`super::dim`] is the function views call.
    pub const SECONDARY: Role = Role {
        name: "secondary",
        size: size::SMALL,
        weight: Weight::Light,
        colour: DIM,
    };

    /// The smallest line on the page: a source, a time, a total that supports a headline.
    pub const CAPTION: Role = Role {
        name: "caption",
        size: size::CAPTION,
        weight: Weight::Light,
        colour: DIM,
    };

    /// A number: a count, a weight, an id. Monospaced, so figures line up in a column.
    pub const NUMBER: Role = Role {
        name: "number",
        size: size::TITLE,
        weight: Weight::Regular,
        colour: INK,
    };

    /// Every role, so a test can walk the scale instead of trusting a list in prose.
    /// A document set in the monospaced face: the licence text, and anything else whose
    /// indentation is part of its meaning.
    ///
    /// It is a role and not a `FontId` literal because the design audit
    /// (`tests/design_audit.rs`) reads a painted line's (size, face) pair and asks the role
    /// table whether the interface produces it. The licence column painted 11 px monospace
    /// with nothing to name it; this is the name.
    pub const DOCUMENT: Role = Role {
        name: "document",
        size: size::CAPTION,
        weight: Weight::Regular,
        colour: DIM,
    };

    pub const ALL: &[Role] = &[
        PAGE_TITLE,
        CARD_TITLE,
        SECTION_LABEL,
        BODY,
        SECONDARY,
        CAPTION,
        NUMBER,
        DOCUMENT,
    ];
}

// The named roles, under the names the views already call. Every one of these existed
// before this change and keeps working; the arguments and the return type are unchanged.

/// The page's title: 21 px, full ink.
pub fn page_title(text: &str) -> RichText {
    role::PAGE_TITLE.text(text)
}

/// A card's title: 15 px, full ink. Was 13 px, which made it pixel-identical to
/// [`body`] — the old version's only difference was a `strong` flag egui ignores.
pub fn card_title(text: &str) -> RichText {
    role::CARD_TITLE.text(text)
}

/// Running text: 13 px, full ink.
pub fn body(text: &str) -> RichText {
    role::BODY.text(text)
}

/// Secondary text: 12 px, dim ink (7.12:1 on the page).
pub fn dim(text: &str) -> RichText {
    role::SECONDARY.text(text)
}

/// A number: 15 px monospaced, full ink.
pub fn number(text: &str) -> RichText {
    role::NUMBER.text(text)
}

/// The name of a group of rows: 13 px, full ink, sentence case as given.
///
/// Added in this pass: no view groups rows yet, so nothing calls it and the build would
/// say so. It is the interface, not dead weight — the first view that lists rows under a
/// heading (the overview's cards, the topics page) should call this instead of writing a
/// capitalised label by hand.
#[allow(dead_code)]
pub fn section_label(text: &str) -> RichText {
    role::SECTION_LABEL.text(text)
}

/// The smallest line on the page: 11 px, dim ink.
///
/// Added in this pass, with [`section_label`]: a source, a time, or a total that
/// supports a headline, one step below [`dim`].
#[allow(dead_code)]
pub fn caption(text: &str) -> RichText {
    role::CAPTION.text(text)
}

// --------------------------------------------------------------------------- contrast

/// WCAG relative luminance of a colour, 0 (black) to 1 (white).
///
/// The sRGB curve, then the coefficients from WCAG 2.2 §relative luminance. For a grey
/// the three channels are equal, so the coefficients sum to one and the answer is the
/// channel's own linearised value — the function stays correct if a hue ever appears,
/// which is the point of computing it rather than hard-coding a ratio.
pub fn luminance(colour: Color32) -> f64 {
    fn channel(value: u8) -> f64 {
        let c = f64::from(value) / 255.0;
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(colour.r()) + 0.7152 * channel(colour.g()) + 0.0722 * channel(colour.b())
}

/// The contrast ratio between two colours, 1.0 (identical) to 21.0 (black on white).
pub fn contrast(a: Color32, b: Color32) -> f64 {
    let (first, second) = (luminance(a), luminance(b));
    let (lighter, darker) = if first >= second { (first, second) } else { (second, first) };
    (lighter + 0.05) / (darker + 0.05)
}

// -------------------------------------------------------------------------------- style

/// One widget state, flat and on the radius.
fn widget(bg_fill: Color32, weak_bg_fill: Color32, bg_stroke: Stroke, fg_stroke: Stroke) -> WidgetVisuals {
    WidgetVisuals {
        bg_fill,
        weak_bg_fill,
        bg_stroke,
        fg_stroke,
        corner_radius: CornerRadius::same(RADIUS),
        // No lift: egui's defaults grow a widget by a pixel on hover and on press, which
        // reads as a shadow at 1 px and as a bounce at the edges of a rail.
        expansion: 0.0,
    }
}

fn widgets() -> Widgets {
    Widgets {
        // Structure inside a panel: separators, an indentation line. Not a control.
        noninteractive: widget(
            PANEL,
            PANEL,
            line(),
            Stroke::new(HAIRLINE_WIDTH, DIM),
        ),
        // A control at rest: a plane of panel with one hairline, dim ink.
        inactive: widget(PANEL, PANEL, line(), Stroke::new(HAIRLINE_WIDTH, DIM)),
        // The pointer is on it: one step up in fill, one step up in ink.
        hovered: widget(
            RAISED,
            RAISED,
            Stroke::new(HAIRLINE_WIDTH, FAINT),
            Stroke::new(HAIRLINE_WIDTH, INK),
        ),
        // Pressed **and** focused — egui has one state for both (`style.rs:1076`). The
        // ring is the brightest thing in the palette, and wider than a hairline, so a
        // keyboard reader can always see where they are.
        active: widget(RAISED, RAISED, focus_stroke(), Stroke::new(HAIRLINE_WIDTH, INK)),
        // A control whose menu is open: raised, still hairline-bordered.
        open: widget(
            RAISED,
            RAISED,
            line(),
            Stroke::new(HAIRLINE_WIDTH, INK),
        ),
    }
}

/// The text styles un-styled widgets get, so a bare `ui.label` and a `ui.button` follow
/// the same ladder as the roles.
fn text_styles() -> std::collections::BTreeMap<TextStyle, FontId> {
    let face = Weight::Light.family();
    [
        (TextStyle::Small, FontId::new(size::CAPTION, face.clone())),
        (TextStyle::Body, FontId::new(size::BODY, face.clone())),
        (TextStyle::Button, FontId::new(size::BODY, face.clone())),
        (TextStyle::Heading, FontId::new(size::PAGE, face.clone())),
        (TextStyle::Monospace, FontId::new(size::TITLE, Weight::Regular.family())),
    ]
    .into()
}

/// The frame's style, from a base: the palette, the ladder, the spacing scale, the flat
/// frame and the focus ring.
///
/// Separate from [`install`] so the whole thing can be asserted without a window: the
/// tests below call `styled(Style::default())` and walk what comes back.
pub fn styled(mut base: Style) -> Style {
    let mut visuals = Visuals::dark();
    visuals.dark_mode = true;

    // Every colour egui knows about, assigned. The six that `Visuals::dark` fills with a
    // hue — hyperlink, warn, error, selection, text cursor, IME composition — are the
    // reason this list is exhaustive rather than "the ones that matter".
    visuals.override_text_color = Some(INK);
    visuals.hyperlink_color = INK; // a link is ink and an underline, never blue
    visuals.warn_fg_color = FOCUS; // a warning is the brightest ink, and the word "warning"
    visuals.error_fg_color = FOCUS; // likewise an error: no red, the sentence carries it
    visuals.text_cursor = TextCursorStyle {
        stroke: Stroke::new(FOCUS_WIDTH, INK),
        ..Default::default()
    };
    visuals.selection = Selection {
        bg_fill: SELECT,
        stroke: Stroke::new(HAIRLINE_WIDTH, INK),
    };
    visuals.faint_bg_color = RAISED;
    visuals.extreme_bg_color = PAGE; // a field is a recess in the page, not a card
    visuals.code_bg_color = PANEL;
    visuals.window_fill = PANEL;
    visuals.window_stroke = line();
    visuals.panel_fill = PAGE;
    visuals.widgets = widgets();

    // egui 0.37 grew three more places a colour can arrive from, and left all three
    // `None`/hued in `Visuals::dark`, so "assigned from the palette" has to include them
    // or the monochrome claim is only true until the first field is focused in a
    // language with an IME.
    //
    // Supporting text ("weak" ink) is DIM, named in the palette, instead of `None`,
    // which would let egui derive it as `text_color × weak_text_alpha` (0.6): a
    // semi-transparent copy of INK that no contrast test in this file can name.
    visuals.weak_text_color = Some(DIM);
    // A field's fill is the page itself: one hairline says "recess", no fill needed, and
    // it stays right if `extreme_bg_color` ever moves.
    visuals.text_edit_bg_color = Some(PAGE);
    // Composition underlines are cursor furniture, so they take the cursor's treatment:
    // the ring's weight, the brightest ink while composing, DIM for the segment waiting
    // its turn. `Visuals::dark` hands out (192, 222, 255) — blue — for the active one.
    visuals.ime_composition = egui::style::ImeComposition {
        // Same stroke as the text cursor: a composition is the cursor growing a tail.
        active_underline_stroke: Stroke::new(FOCUS_WIDTH, INK),
        inactive_underline_stroke: Stroke::new(FOCUS_WIDTH, DIM),
        ..egui::style::ImeComposition::default() // keeps the platform's `legacy_visuals` default
    };

    // Flat: no shadow, no "topmost" halo, no rounding above the one radius.
    visuals.window_shadow = egui::epaint::Shadow::NONE;
    visuals.popup_shadow = egui::epaint::Shadow::NONE;
    visuals.window_highlight_topmost = false;
    visuals.window_corner_radius = CornerRadius::same(RADIUS);
    visuals.menu_corner_radius = CornerRadius::same(RADIUS);
    visuals.striped = false;

    // Geometry on the scale. `clip_rect_margin` was `space::XS` until egui 0.37: the field
    // still exists, is documented "now unused and has no effect", and has no replacement
    // knob, so nothing grows a scroll area's content clip rect any more. Reported here
    // rather than papered over.
    visuals.resize_corner_size = space::MD;

    base.visuals = visuals;
    base.text_styles = text_styles();

    base.spacing.item_spacing = vec2(space::MD, space::SM);
    base.spacing.button_padding = vec2(space::SM, space::XS);
    base.spacing.window_margin = Margin::same(space::SM as i8);
    base.spacing.menu_margin = Margin::same(space::MD as i8);
    base.spacing.indent = space::LG;
    // A control is at least 24 by 24: WCAG 2.2 asks a target be 24 px across, and the
    // pointer is not the only way in. Both numbers are on the scale.
    base.spacing.interact_size = vec2(space::XL, space::XL);
    base.spacing.icon_width = space::LG;
    base.spacing.icon_width_inner = space::SM;
    base.spacing.icon_spacing = space::XS;
    base.spacing.text_edit_width = space::COLUMN;
    base.spacing.slider_width = space::COLUMN;
    base.spacing.combo_width = space::COLUMN;
    base.spacing.scroll.bar_width = space::SM;
    base.spacing.scroll.handle_min_length = space::XL;
    base.spacing.scroll.bar_inner_margin = space::XS;
    base.spacing.scroll.bar_outer_margin = space::XS;
    base.spacing.scroll.floating_width = space::SM;
    base.spacing.scroll.floating_allocated_width = space::MD;
    base.spacing.scroll.foreground_color = true;
    // No fade at the edge of a scroll area. egui 0.37 paints it as a two-stop mesh
    // (ScrollFadeStyle { strength: 0.5, size: 20.0 }), i.e. the one gradient in a window
    // whose whole language is flat ink — measured five times, at the top of the Overview,
    // the Articles column and this licence screen. The scroll bar is the affordance;
    // nothing needs to fade to say there is more.
    base.spacing.scroll.fade.strength = 0.0;

    base
}

/// The house language, checked at install time in a debug build.
///
/// The same claims the tests below make, cheap enough to run on every launch: the
/// palette is greyscale, the ladder ascends, every spacing number is a whole number of
/// units, every role sits on a rung of the ladder, the two weights are two different
/// faces, and the focus ring clears the 3:1 WCAG asks of a control boundary.
///
/// It exists as well as the tests because the tests run in CI and this runs on the
/// developer's machine — a colour, a size or a gap typed into this file is wrong the
/// first time the window opens, not the first time the suite does.
fn self_check() {
    for (name, colour) in ALL {
        debug_assert!(
            colour.r() == colour.g() && colour.g() == colour.b(),
            "{name} is not greyscale"
        );
    }
    for pair in LADDER.windows(2) {
        debug_assert!(pair[1] > pair[0], "the type ladder is out of order: {pair:?}");
    }
    for (name, value) in SPACING {
        debug_assert!(
            (value / space::UNIT).fract() == 0.0,
            "{name} ({value} px) is not a whole number of {} px units",
            space::UNIT
        );
    }
    for role in role::ALL {
        debug_assert!(LADDER.contains(&role.size), "{} is off the ladder", role.name);
    }
    debug_assert_ne!(Weight::Light.face(), Weight::Regular.face(), "one face, two weights");
    debug_assert!(
        contrast(FOCUS, PAGE) >= 3.0,
        "the focus ring is invisible: {:.2}:1 on the page",
        contrast(FOCUS, PAGE)
    );
    // and the scale ascends, so a gap cannot be smaller than the one below it
    let steps = [space::XS, space::SM, space::MD, space::LG, space::XL, space::XXL];
    for pair in steps.windows(2) {
        debug_assert!(pair[1] > pair[0], "the spacing scale is out of order: {pair:?}");
    }
}

/// Install the frame's style on a context. Call once, from the app's constructor.
pub fn install(ctx: &egui::Context) {
    self_check();
    // egui 0.37 keeps one style per theme and no longer has `style()`/`set_style()`.
    // Both slots are given the house language, because `ThemePreference` defaults to
    // `System`: a reader on a light-mode desktop would otherwise fall back to egui's
    // *coloured* defaults for tooltips and popups, which would break the one rule this
    // file exists to enforce.
    ctx.all_styles_mut(|style| *style = styled(style.clone()));
}

// -------------------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    /// The style as the window installs it, without a window.
    fn style() -> Style {
        styled(Style::default())
    }

    fn assert_grey(what: &str, colour: Color32) {
        assert_eq!(
            (colour.r(), colour.g(), colour.b(), colour.a()),
            (colour.r(), colour.r(), colour.r(), 255),
            "{what} is not greyscale: {:?}",
            (colour.r(), colour.g(), colour.b())
        );
    }

    /// Every colour the installed `Visuals` carries, by name.
    ///
    /// Written out by hand on purpose: this is the list an audit reads. A field egui
    /// fills with a hue and this theme forgets shows up as a failure, not as a hue in a
    /// screenshot nobody diffed.
    fn visuals_colours(visuals: &Visuals) -> Vec<(&'static str, Color32)> {
        let mut out: Vec<(&'static str, Color32)> = vec![
            ("override_text_color", visuals.override_text_color.expect("set")),
            ("hyperlink_color", visuals.hyperlink_color),
            ("warn_fg_color", visuals.warn_fg_color),
            ("error_fg_color", visuals.error_fg_color),
            ("faint_bg_color", visuals.faint_bg_color),
            ("extreme_bg_color", visuals.extreme_bg_color),
            ("code_bg_color", visuals.code_bg_color),
            ("window_fill", visuals.window_fill),
            ("window_stroke", visuals.window_stroke.color),
            ("panel_fill", visuals.panel_fill),
            ("selection.bg_fill", visuals.selection.bg_fill),
            ("selection.stroke", visuals.selection.stroke.color),
            ("text_cursor.stroke", visuals.text_cursor.stroke.color),
            ("window_shadow", visuals.window_shadow.color),
            ("popup_shadow", visuals.popup_shadow.color),
            // egui 0.37's three new colour slots. `expect("set")` on the two `Option`s is
            // the assertion: left at `None`, egui derives a colour this file never named
            // (`text_color × weak_text_alpha` for weak ink), which is the state that looks
            // monochrome and is not a palette member.
            ("weak_text_color", visuals.weak_text_color.expect("set")),
            ("text_edit_bg_color", visuals.text_edit_bg_color.expect("set")),
            (
                "ime_composition.active_underline_stroke",
                visuals.ime_composition.active_underline_stroke.color,
            ),
            (
                "ime_composition.inactive_underline_stroke",
                visuals.ime_composition.inactive_underline_stroke.color,
            ),
        ];
        let states: [(&'static str, &WidgetVisuals); 5] = [
            ("noninteractive", &visuals.widgets.noninteractive),
            ("inactive", &visuals.widgets.inactive),
            ("hovered", &visuals.widgets.hovered),
            ("active", &visuals.widgets.active),
            ("open", &visuals.widgets.open),
        ];
        for (name, state) in states {
            out.push((name, state.bg_fill));
            out.push((name, state.weak_bg_fill));
            out.push((name, state.bg_stroke.color));
            out.push((name, state.fg_stroke.color));
        }
        out
    }

    // ------------------------------------------------------------------------ palette

    #[test]
    fn every_colour_in_the_palette_is_greyscale() {
        assert!(ALL.len() >= 10, "the palette lost colours: {}", ALL.len());
        for (name, colour) in ALL {
            assert_grey(name, *colour);
        }
        // and the palette is the only source: the names the views use are in it
        for name in ["PAGE", "PANEL", "RAISED", "HAIRLINE", "HAIRLINE_SOFT", "INK", "DIM", "FAINT"] {
            assert!(ALL.iter().any(|(known, _)| *known == name), "{name} left ALL");
        }
    }

    #[test]
    fn the_installed_visuals_carry_no_hue() {
        // egui's own dark visuals set blue links, an orange warning, a red error and a
        // blue text cursor; this asserts the theme overrides all four, and everything
        // else besides.
        let visuals = style().visuals;
        for (name, colour) in visuals_colours(&visuals) {
            if colour.a() == 0 {
                continue; // an absent shadow is transparent on purpose
            }
            assert_grey(name, colour);
        }
    }

    // --------------------------------------------------------------------- egui 0.37

    #[test]
    fn the_new_colour_slots_hold_palette_members() {
        let visuals = style().visuals;

        // Weak ink (a hint, a supporting line) is DIM — a member of the palette with a
        // ratio this file measures — and not `None`, which would make egui derive
        // `text_color × weak_text_alpha` (INK at 60 % alpha): a grey no test here could
        // name and no view could get right twice.
        assert_eq!(
            visuals.weak_text_color,
            Some(DIM),
            "weak ink is not the palette's DIM"
        );
        let weak = visuals.weak_text_color();
        assert_grey("weak_text_color()", weak);
        assert!(
            ALL.iter().any(|(_, colour)| *colour == weak),
            "weak ink resolves to a colour outside the palette: {weak:?}"
        );
        assert!(
            contrast(weak, PAGE) >= 4.5,
            "weak ink on the page is {:.2}:1, below the body-text floor",
            contrast(weak, PAGE)
        );

        // A field's fill is the page by name, not `extreme_bg_color` by inheritance.
        assert_eq!(visuals.text_edit_bg_color, Some(PAGE));

        // Composition underlines get the cursor's treatment rather than egui's blue: the
        // ring's weight, ink while composing, dim for the segment waiting its turn.
        let ime = visuals.ime_composition;
        assert_eq!(ime.active_underline_stroke.color, INK);
        assert_eq!(ime.inactive_underline_stroke.color, DIM);
        for (name, stroke) in [
            ("active", ime.active_underline_stroke),
            ("inactive", ime.inactive_underline_stroke),
        ] {
            assert_eq!(
                stroke.width, FOCUS_WIDTH,
                "the {name} composition underline is {} px, not the cursor's weight",
                stroke.width
            );
            assert!(
                contrast(stroke.color, PAGE) >= 3.0,
                "the {name} composition underline is {:.2}:1 on the page",
                contrast(stroke.color, PAGE)
            );
        }
        println!(
            "weak ink on the page {:.2}:1; composition underline active {:.2}:1, inactive {:.2}:1",
            contrast(weak, PAGE),
            contrast(ime.active_underline_stroke.color, PAGE),
            contrast(ime.inactive_underline_stroke.color, PAGE)
        );
    }

    #[test]
    fn strong_is_a_colour_and_it_has_nothing_to_say_here() {
        // `.strong()` resolves to `widgets.active.text_color()` (egui 0.37,
        // `style.rs:1257`), i.e. it is a colour, not a weight. With this palette that
        // colour *is* body ink, so a view reaching for `RichText::strong()` gets exactly
        // the pixels of a view that does not. Pinned so that a later change to the focus
        // state's ink — `active.fg_stroke` — cannot quietly turn `.strong()` into the
        // fake emphasis this theme was written to remove.
        let visuals = style().visuals;
        assert_eq!(
            visuals.strong_text_color(),
            visuals.text_color(),
            "`.strong()` now changes colour: hierarchy has moved into a flag again"
        );
    }

    // --------------------------------------------------------------------- form, and flat

    #[test]
    fn nothing_is_rounded_above_two_pixels() {
        let visuals = style().visuals;
        let zero = CornerRadius::ZERO;
        let radii: [(&str, CornerRadius); 7] = [
            ("window", visuals.window_corner_radius),
            ("menu", visuals.menu_corner_radius),
            ("noninteractive", visuals.widgets.noninteractive.corner_radius),
            ("inactive", visuals.widgets.inactive.corner_radius),
            ("hovered", visuals.widgets.hovered.corner_radius),
            ("active", visuals.widgets.active.corner_radius),
            ("open", visuals.widgets.open.corner_radius),
        ];
        for (name, radius) in radii {
            for (corner, value) in [
                ("nw", radius.nw),
                ("ne", radius.ne),
                ("sw", radius.sw),
                ("se", radius.se),
            ] {
                assert!(
                    value <= RADIUS,
                    "{name}.{corner} is rounded {value} px, more than the house rule allows ({RADIUS})"
                );
            }
            assert_ne!(radius, zero, "{name} lost its radius entirely — the rule is `at most`, not `none`");
        }
    }

    #[test]
    fn nothing_fakes_depth() {
        let visuals = style().visuals;
        for (name, shadow) in [
            ("window_shadow", visuals.window_shadow),
            ("popup_shadow", visuals.popup_shadow),
        ] {
            assert_eq!(shadow.blur, 0, "{name} blurs");
            assert_eq!(shadow.spread, 0, "{name} spreads");
            assert_eq!(shadow.offset, [0, 0], "{name} is offset");
            assert_eq!(shadow.color.a(), 0, "{name} is painted");
        }
        for (name, state) in [
            ("noninteractive", &visuals.widgets.noninteractive),
            ("inactive", &visuals.widgets.inactive),
            ("hovered", &visuals.widgets.hovered),
            ("active", &visuals.widgets.active),
            ("open", &visuals.widgets.open),
        ] {
            assert_eq!(state.expansion, 0.0, "{name} grows on interaction");
            // A thicker glyph stroke is a fake bold: egui 0.37 draws hovered text with a
            // 1.5 px stroke and active text with 2 px, which reads as weight the font does
            // not have. Hierarchy on hover comes from fill and ink, not from a fatter pen.
            assert_eq!(
                state.fg_stroke.width, HAIRLINE_WIDTH,
                "{name} text is drawn with a {} px pen",
                state.fg_stroke.width
            );
        }
        assert!(!visuals.window_highlight_topmost, "a halo on the topmost window");
        assert!(!visuals.striped, "striping is a second surface nobody asked for");
    }

    // --------------------------------------------------------------------------- focus

    #[test]
    fn the_focus_ring_is_visible_on_the_page() {
        // The brief asks for a ring visible on #0a0a0a; this asserts the page *is*
        // #0a0a0a before measuring anything against it, so the ratio below cannot be
        // measuring a different background than the one the design names.
        assert_eq!(PAGE, Color32::from_rgb(0x0a, 0x0a, 0x0a), "PAGE is not #0a0a0a");
        let ratio = contrast(FOCUS, PAGE);
        assert!(
            ratio >= 7.0,
            "the focus ring is {ratio:.2}:1 on the page; WCAG asks 3:1 for a control boundary \
             and house style asks for more than body ink"
        );
        // brighter than body ink, so "focused" cannot read as "ordinary text"
        assert!(
            luminance(FOCUS) > luminance(INK),
            "the ring is not brighter than ink: {:.3} vs {:.3}",
            luminance(FOCUS),
            luminance(INK)
        );
        // and the state egui paints for keyboard focus is the one that wears it
        let active = style().visuals.widgets.active;
        assert_eq!(active.bg_stroke.color, FOCUS, "the focus state does not wear the ring");
        assert!(
            active.bg_stroke.width >= FOCUS_WIDTH,
            "the ring is {} px, thinner than the decision ({FOCUS_WIDTH})",
            active.bg_stroke.width
        );
        // printed, so the number in the report is a measurement
        println!("focus ring: {ratio:.2}:1 on the page, {} px", active.bg_stroke.width);
    }

    #[test]
    fn every_grey_that_carries_text_is_legible() {
        // 4.5:1 is WCAG's floor for body text; the theme's ink is asserted against every
        // surface a view can put it on, not just the page. The numbers are printed so a
        // doc comment that quotes one has a measurement behind it.
        for surface in [("page", PAGE), ("panel", PANEL), ("raised", RAISED), ("select", SELECT)] {
            let (name, fill) = surface;
            let ink = contrast(INK, fill);
            let dim = contrast(DIM, fill);
            let faint = contrast(FAINT, fill);
            println!(
                "on {name}: ink {ink:.2}:1, dim {dim:.2}:1, faint {faint:.2}:1, ring {:.2}:1",
                contrast(FOCUS, fill)
            );
            assert!(ink >= 4.5, "ink on {name} is {ink:.2}:1");
            assert!(dim >= 4.5, "dim ink on {name} is {dim:.2}:1");
        }
        // FAINT is a stroke, not ink: a boundary needs 3:1
        for surface in [("page", PAGE), ("panel", PANEL), ("raised", RAISED)] {
            let (name, fill) = surface;
            let faint = contrast(FAINT, fill);
            assert!(faint >= 3.0, "a faint boundary on {name} is {faint:.2}:1");
        }
        // and it stops being enough on a selected row (2.59:1), which is why the palette
        // says a boundary drawn on a selection uses DIM. Pinned so the sentence and the
        // measurement cannot drift apart.
        let on_selection = contrast(FAINT, SELECT);
        assert!(
            on_selection < 3.0,
            "a faint boundary now clears 3:1 on a selected row ({on_selection:.2}:1) — \
             the palette's note about DIM is out of date"
        );
        assert!(contrast(DIM, SELECT) >= 3.0, "DIM does not work as a boundary on a selection");
    }

    // ---------------------------------------------------------------------------- type

    #[test]
    fn the_type_scale_is_one_ladder() {
        for pair in LADDER.windows(2) {
            let (small, large) = (pair[0], pair[1]);
            assert!(large > small, "the ladder is not ordered: {small} then {large}");
            let step = large / small;
            assert!(
                (1.05..=1.6).contains(&step),
                "a step of {step:.3} between {small} and {large} is not a step a reader sees"
            );
        }
        for role in role::ALL {
            assert!(
                LADDER.contains(&role.size),
                "{} is {} px, which is not on the ladder",
                role.name,
                role.size
            );
            assert_grey(role.name, role.colour);
        }
        // Two roles may share a rung on purpose — a section label is body-sized, and the
        // space above it is what makes it a label. The pair that must never be identical
        // is a title and the text it titles: a card title that renders exactly like body
        // text is not a title, and that was the old theme's `card_title` (13 px, and a
        // `strong` flag egui ignores).
        let card = (role::CARD_TITLE.size, role::CARD_TITLE.colour, role::CARD_TITLE.weight);
        let body = (role::BODY.size, role::BODY.colour, role::BODY.weight);
        assert_ne!(card, body, "a card title renders exactly like body text");
        assert!(card.0 > body.0, "the title is not larger than the text it titles");
    }

    #[test]
    fn a_role_is_a_face_that_ships() {
        // "real weights": the family a weight names is one epaint actually loads, and
        // the two weights are two different faces.
        let bundled = egui::FontDefinitions::builtin_font_names();
        for weight in [Weight::Light, Weight::Regular] {
            assert!(
                bundled.contains(&weight.face()),
                "{} is not a bundled face: {bundled:?}",
                weight.face()
            );
        }
        assert_ne!(Weight::Light.face(), Weight::Regular.face());
        assert_ne!(Weight::Light.family(), Weight::Regular.family());
        assert_eq!(style().text_styles[&TextStyle::Body].size, size::BODY);
        assert_eq!(style().text_styles[&TextStyle::Monospace].size, size::TITLE);
    }

    #[test]
    fn no_role_pretends_to_a_weight_it_does_not_have() {
        // egui 0.31 reads the `strong` flag only to pick a colour, so setting it next to
        // an explicit colour is a no-op that reads as hierarchy. The theme never sets it.
        // The scan covers the shipped half of this file — everything above the test
        // module — so that this assertion can name the flag it is looking for.
        let source = include_str!("theme.rs");
        let shipped = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(
            !shipped.contains(".strong()"),
            "a role sets egui's strong flag, which changes no weight — use a size or a colour"
        );
        // and a role is told apart by a frame or a weight, not by a flag: the numbers and
        // the caption are checked here so the roles that carry the scale are pinned too
        let body = role::BODY;
        let caption = role::CAPTION;
        assert!(caption.size < body.size, "the caption is not the smallest role");
        assert_eq!(role::SECTION_LABEL.size, size::BODY, "a section label is body-sized");
        assert_eq!(
            role::SECTION_LABEL.colour, INK,
            "a section label is ink — a group name is read, not whispered"
        );
        assert_eq!(role::NUMBER.weight, Weight::Regular, "a number is set in the numeric face");
    }

    #[test]
    fn a_label_keeps_the_case_it_is_given() {
        // "sentence case" is the caller's job and the theme's promise: nothing here
        // upper-cases, title-cases or letter-spaces a string.
        for role in role::ALL {
            let given = "Mixed Case — mixed case";
            assert_eq!(role.text(given).text(), given, "{} changed its text", role.name);
        }
        // no role adds letter spacing either: the old UI's eyebrows did it with tracking
        let rendered = role::SECTION_LABEL.text("Recent");
        assert_eq!(rendered.text(), "Recent");
    }

    // ------------------------------------------------------------------------- spacing

    #[test]
    fn the_spacing_scale_is_one_unit() {
        assert!(!SPACING.is_empty());
        for (name, value) in SPACING {
            assert!(*value > 0.0, "{name} is {value}");
            let units = value / space::UNIT;
            assert_eq!(
                units,
                units.round(),
                "{name} is {value} px, which is not a whole number of {} px units",
                space::UNIT
            );
        }
        // the named steps are a scale: distinct, ascending
        let steps = [
            space::XS,
            space::SM,
            space::MD,
            space::LG,
            space::XL,
            space::XXL,
        ];
        for pair in steps.windows(2) {
            assert!(pair[1] > pair[0], "the spacing scale is not ordered: {pair:?}");
        }
        assert_eq!(space::UNIT as u32 as f32 * 8.0, space::XXL);
        assert_eq!(space::COLUMN / space::UNIT, 24.0, "a field column is 24 units");
    }

    #[test]
    fn the_style_carries_the_scale_and_not_a_number_of_its_own() {
        let spacing = style().spacing;
        assert_eq!(spacing.item_spacing, vec2(space::MD, space::SM));
        assert_eq!(spacing.button_padding, vec2(space::SM, space::XS));
        assert_eq!(spacing.window_margin, Margin::same(space::SM as i8));
        assert_eq!(spacing.menu_margin, Margin::same(space::MD as i8));
        assert_eq!(spacing.indent, space::LG);
        assert_eq!(spacing.interact_size, vec2(space::XL, space::XL));
        // a target is at least 24 px, on the scale
        assert!(spacing.interact_size.x >= 24.0 && spacing.interact_size.y >= 24.0);
        assert_eq!(spacing.icon_width, space::LG);
        assert_eq!(spacing.icon_width_inner, space::SM);
        assert_eq!(spacing.icon_spacing, space::XS);
        assert_eq!(spacing.text_edit_width, space::COLUMN);
        assert_eq!(spacing.slider_width, space::COLUMN);
        assert_eq!(spacing.combo_width, space::COLUMN);
        assert_eq!(spacing.scroll.bar_width, space::SM);
        // every number the style carries is either on the scale or a length no one reads
        // as space (a slider's rail height, a tooltip's wrapping width)
        for value in [
            spacing.indent,
            spacing.icon_width,
            spacing.icon_width_inner,
            spacing.icon_spacing,
            spacing.text_edit_width,
            spacing.scroll.bar_width,
            spacing.scroll.handle_min_length,
            spacing.scroll.bar_inner_margin,
            spacing.scroll.bar_outer_margin,
        ] {
            let units = value / space::UNIT;
            assert_eq!(units, units.round(), "{value} px is not on the spacing scale");
        }
    }

    // ----------------------------------------------------------------- the public names

    #[test]
    fn every_name_the_views_call_still_exists_and_still_answers() {
        // The pinning test for "keep every existing name working": this compiles only if
        // every constant, function and signature the views (and app.rs) already use is
        // still here, unchanged.
        let stroke: Stroke = line();
        assert_eq!(stroke.color, HAIRLINE);
        assert_eq!(stroke.width, HAIRLINE_WIDTH);
        let _: &[(&str, Color32)] = ALL;
        let surfaces = [PAGE, PANEL, RAISED, HAIRLINE, HAIRLINE_SOFT, INK, DIM, FAINT];
        for colour in surfaces {
            assert_grey("a surface", colour);
        }
        let roles: [RichText; 7] = [
            page_title("a page"),
            card_title("a card"),
            section_label("Recent"),
            body("running text"),
            dim("a hint"),
            caption("a source"),
            number("42"),
        ];
        for text in roles {
            assert!(!text.text().is_empty());
        }
        // and the new names are the ones the design asked for
        assert_eq!(caption("x").text(), "x");
        assert_eq!(number("1 234").text(), "1 234");
    }

    #[test]
    fn contrast_maths_matches_the_wcag_anchors() {
        // The two ends of the scale, so a wrong coefficient cannot pass quietly.
        let black = Color32::from_gray(0);
        let white = Color32::from_gray(255);
        assert!((contrast(white, black) - 21.0).abs() < 0.01, "{}", contrast(white, black));
        assert!((contrast(black, black) - 1.0).abs() < 0.001);
        assert!((luminance(white) - 1.0).abs() < 0.001);
        assert!(luminance(black).abs() < 0.001);
        // a mid grey's luminance is the sRGB curve, not the byte value
        let half = Color32::from_gray(128);
        assert!(luminance(half) < 0.25, "{}", luminance(half));
    }
}
