//! Placeholder until generated.

pub(super) struct ZoneEntry {
    pub(super) id: &'static str,
    pub(super) canonical: &'static str,
    pub(super) standard: &'static str,
    pub(super) daylight: &'static str,
}

pub(super) static ZONES: &[ZoneEntry] = &[ZoneEntry {
    id: "UTC",
    canonical: "UTC",
    standard: "Coordinated Universal Time",
    daylight: "Coordinated Universal Time",
}];

pub(super) fn lookup(id: &str) -> Option<&'static ZoneEntry> {
    ZONES.iter().find(|entry| entry.id.eq_ignore_ascii_case(id))
}
