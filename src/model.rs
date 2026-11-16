#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub dock_x: i32,
    pub dock_width: i32,
    pub tray_x: i32,
    pub tray_width: i32,
    pub y: i32,
    pub height: i32,
    pub slot: i32,
    pub capacity: usize,
    pub overflow: bool,
}

pub fn layout(width: i32, height: i32, dpi: u32, apps: usize) -> Option<Layout> {
    if !(48..=768).contains(&dpi) || height <= 0 {
        return None;
    }
    let px = |n: i32| n * dpi as i32 / 96;
    let (bar_height, slot, padding, margin, gap, tray_width) =
        (px(48), px(44), px(10), px(8), px(16), px(260));
    let tray_x = width - margin - tray_width;
    let available = tray_x - gap - margin;
    let slots = ((available - 2 * padding) / slot).max(0) as usize;
    if slots < 3 || height < bar_height + px(2) {
        return None;
    }
    let overflow = apps + 1 > slots;
    let capacity = if overflow { slots - 2 } else { apps };
    let used_slots = 1 + capacity + usize::from(overflow);
    let dock_width = 2 * padding + used_slots as i32 * slot;
    let dock_x = ((width - dock_width) / 2)
        .min(tray_x - gap - dock_width)
        .max(margin);
    Some(Layout {
        dock_x,
        dock_width,
        tray_x,
        tray_width,
        y: height - px(2) - bar_height,
        height: bar_height,
        slot,
        capacity,
        overflow,
    })
}

pub fn group_key(path: &str, app_id: &str) -> String {
    format!("{}|{}", path.to_lowercase(), app_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dock_is_48_high_and_two_from_bottom() {
        let l = layout(1920, 1080, 96, 8).expect("normal desktop");
        assert_eq!(l.height, 48);
        assert_eq!(l.y + l.height, 1078);
        assert!(!l.overflow);
        assert_eq!(l.capacity, 8);
    }

    #[test]
    fn many_apps_leave_space_for_tray_and_overflow() {
        for dpi in [96, 120, 144, 192] {
            for width in [800, 1024, 1366, 1920, 3840] {
                if let Some(l) = layout(width, 1080, dpi, 100) {
                    assert!(l.dock_x >= 0);
                    assert!(l.dock_x + l.dock_width + 16 * dpi as i32 / 96 <= l.tray_x);
                    assert!(l.tray_x + l.tray_width <= width);
                    assert!(l.overflow);
                    assert!(l.capacity < 100);
                    assert!(l.slot >= 44);
                }
            }
        }
    }

    #[test]
    fn scaling_preserves_bottom_gap() {
        let l = layout(2560, 1440, 144, 10).unwrap();
        assert_eq!(l.height, 72);
        assert_eq!(l.y + l.height, 1437);
    }

    #[test]
    fn impossible_display_is_rejected() {
        assert!(layout(100, 100, 96, 5).is_none());
        assert!(layout(1920, 0, 96, 5).is_none());
        assert!(layout(1920, 1080, 0, 5).is_none());
    }

    #[test]
    fn application_identity_is_case_insensitive() {
        assert_eq!(group_key("C:\\APP.EXE", ""), group_key("c:\\app.exe", ""));
    }

    #[test]
    fn packaged_apps_share_host_but_not_identity() {
        assert_ne!(
            group_key("host.exe", "Store!App"),
            group_key("host.exe", "Mail!App")
        );
    }
}
