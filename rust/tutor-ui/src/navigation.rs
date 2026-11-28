use wasm_bindgen::{closure::Closure, JsCast};
use yew::Callback;

pub struct RouteListener {
    window: web_sys::Window,
    listener: Closure<dyn FnMut(web_sys::Event)>,
}

impl RouteListener {
    pub fn new(callback: Callback<AppView>) -> Option<Self> {
        let window = web_sys::window()?;
        let listener = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            let hash = web_sys::window()
                .and_then(|window| window.location().hash().ok())
                .unwrap_or_default();
            callback.emit(AppView::from_hash(&hash));
        });
        window
            .add_event_listener_with_callback("hashchange", listener.as_ref().unchecked_ref())
            .ok()?;
        Some(Self { window, listener })
    }
}

impl Drop for RouteListener {
    fn drop(&mut self) {
        let _ = self.window.remove_event_listener_with_callback(
            "hashchange",
            self.listener.as_ref().unchecked_ref(),
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppView {
    Home,
    Voice,
    Activities,
    Conversation,
    Records,
}

impl AppView {
    pub fn hash(self) -> &'static str {
        match self {
            Self::Home => "#/home",
            Self::Voice => "#/voice",
            Self::Activities => "#/activities",
            Self::Conversation => "#/conversation",
            Self::Records => "#/records",
        }
    }

    pub fn from_hash(hash: &str) -> Self {
        match hash {
            "#/voice" => Self::Voice,
            "#/activities" => Self::Activities,
            "#/conversation" | "#learning-conversation" => Self::Conversation,
            "#/records" | "#learning-records" => Self::Records,
            _ => Self::Home,
        }
    }

    pub fn current() -> Self {
        web_sys::window()
            .and_then(|window| window.location().hash().ok())
            .map_or(Self::Home, |hash| Self::from_hash(&hash))
    }

    pub fn write_history(self, replace: bool) {
        let Some(history) = web_sys::window().and_then(|window| window.history().ok()) else {
            return;
        };
        let result = if replace {
            history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(self.hash()))
        } else {
            history.push_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(self.hash()))
        };
        let _ = result;
    }
}

#[cfg(test)]
mod tests {
    use super::AppView;

    #[test]
    fn routes_have_stable_addresses_and_legacy_aliases() {
        assert_eq!(AppView::from_hash("#/home"), AppView::Home);
        assert_eq!(AppView::from_hash("#/activities"), AppView::Activities);
        assert_eq!(AppView::from_hash("#/conversation"), AppView::Conversation);
        assert_eq!(AppView::from_hash("#/records"), AppView::Records);
        assert_eq!(AppView::from_hash("#learning-records"), AppView::Records);
        assert_eq!(
            AppView::from_hash("#learning-conversation"),
            AppView::Conversation
        );
        assert_eq!(AppView::from_hash("#unknown"), AppView::Home);
        for view in [
            AppView::Home,
            AppView::Voice,
            AppView::Activities,
            AppView::Conversation,
            AppView::Records,
        ] {
            assert_eq!(AppView::from_hash(view.hash()), view);
        }
    }
}
