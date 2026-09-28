//! What the on-screen popups show (drawn by each platform's osd module).

#[derive(Clone, Default, Debug)]
pub enum Icon {
    #[default]
    None,
    Exe(String), // full path
    Speaker,
    Mic,
    Profile,
    Light,
}

/// One control on the cheat sheet.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct SheetItem {
    /// "K1", "S3"
    pub tag: String,
    /// Its label, or what turning it does; empty when unused.
    pub title: String,
    /// What turning does (when the title is a label), then its press, double press and hold.
    pub lines: Vec<String>,
    /// The control's light color.
    pub color: [u8; 3],
    pub knob: bool,
}

#[derive(Clone, Default, Debug)]
pub struct Info {
    pub title: String,
    pub exe_name: bool, // title is a fallback; prefer the exe's friendly name
    pub icon: Icon,
    pub level: Option<f32>,
    /// Where the physical control is while it hasn't picked up the volume yet.
    pub marker: Option<f32>,
    pub muted: bool,
    pub hint: String,
    pub top: bool,
    /// Cheat sheet cards, one per control. Shown instead of the volume popup.
    pub sheet: Vec<SheetItem>,
    /// How long to show it (0 = the usual moment).
    pub stay_ms: u64,
    /// Fade out the cheat sheet now.
    pub hide: bool,
}
