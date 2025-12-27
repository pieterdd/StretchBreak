fn main() {
    relm4_icons_build::bundle_icons(
        // Name of the file that will be generated at `OUT_DIR`
        "icon_names.rs",
        // Optional app ID
        Some("io.github.pieterdd.StretchBreak"),
        // Custom base resource path
        None::<&str>,
        // Directory with custom icons (if any)
        None::<&str>,
        // List of icons to include
        ["timer", "settings", "snooze-filled", "stopwatch"],
    );
}
