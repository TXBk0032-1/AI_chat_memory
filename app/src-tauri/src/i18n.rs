use crate::models::SupportedLocale;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeText {
    pub app_title: &'static str,
    pub open: &'static str,
    pub quit: &'static str,
}

pub fn native_text(locale: SupportedLocale) -> NativeText {
    match locale {
        SupportedLocale::ZhCn => NativeText {
            app_title: "对话归档",
            open: "打开对话归档",
            quit: "退出",
        },
        SupportedLocale::EnUs => NativeText {
            app_title: "Conversation Archive",
            open: "Open Conversation Archive",
            quit: "Quit",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SupportedLocale;

    #[test]
    fn native_dictionary_contains_window_and_tray_copy() {
        assert_eq!(
            native_text(SupportedLocale::EnUs).app_title,
            "Conversation Archive"
        );
        assert_eq!(native_text(SupportedLocale::EnUs).quit, "Quit");
        assert_eq!(native_text(SupportedLocale::ZhCn).open, "打开对话归档");
    }
}
