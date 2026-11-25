//! Browser resources used only by CameraPreview; every resource has an owner.
use crate::preview_stream::{MAX_JPEG, MAX_META};
use wasm_bindgen::{JsCast, closure::Closure};
use web_sys::{
    BinaryType, Blob, BlobPropertyBag, Document, Event, HtmlImageElement, MessageEvent, Url,
    WebSocket,
};
use yew::Callback;

pub struct PreviewSocket {
    socket: WebSocket,
    _message: Closure<dyn FnMut(MessageEvent)>,
    _ended: Closure<dyn FnMut(Event)>,
}
impl PreviewSocket {
    pub fn connect(receive: Callback<Result<Vec<u8>, String>>) -> Result<Self, String> {
        let location = web_sys::window().ok_or("浏览器窗口不可用")?.location();
        let scheme = if location.protocol().map_err(|_| "预览地址不可用")? == "https:" {
            "wss"
        } else {
            "ws"
        };
        let host = location.host().map_err(|_| "预览地址不可用")?;
        let socket = WebSocket::new(&format!("{scheme}://{host}/api/video/stream"))
            .map_err(|_| "无法建立二进制预览连接")?;
        socket.set_binary_type(BinaryType::Arraybuffer);
        let messages = receive.clone();
        let message = Closure::wrap(Box::new(move |event: MessageEvent| {
            let result = event
                .data()
                .dyn_into::<js_sys::ArrayBuffer>()
                .map_err(|_| "二进制预览收到非二进制消息".to_owned())
                .and_then(|data| {
                    if data.byte_length() as usize > 4 + MAX_META + MAX_JPEG {
                        Err("预览消息超过大小限制".into())
                    } else {
                        Ok(js_sys::Uint8Array::new(&data).to_vec())
                    }
                });
            messages.emit(result);
        }) as Box<dyn FnMut(MessageEvent)>);
        let ended = Closure::wrap(Box::new(move |_: Event| {
            receive.emit(Err("二进制预览连接已断开，请重试或选择兼容预览。".into()));
        }) as Box<dyn FnMut(Event)>);
        socket.set_onmessage(Some(message.as_ref().unchecked_ref()));
        socket.set_onerror(Some(ended.as_ref().unchecked_ref()));
        socket.set_onclose(Some(ended.as_ref().unchecked_ref()));
        Ok(Self {
            socket,
            _message: message,
            _ended: ended,
        })
    }
    pub fn ack(&self, value: &str) -> Result<(), String> {
        self.socket
            .send_with_str(value)
            .map_err(|_| "无法确认预览画面".into())
    }
}
impl Drop for PreviewSocket {
    fn drop(&mut self) {
        self.socket.set_onmessage(None);
        self.socket.set_onerror(None);
        self.socket.set_onclose(None);
        let _ = self.socket.close();
    }
}

pub struct ImageSource {
    pub url: String,
    object_url: bool,
}
impl ImageSource {
    pub fn jpeg(bytes: &[u8]) -> Result<Self, String> {
        let parts = js_sys::Array::new();
        parts.push(&js_sys::Uint8Array::from(bytes));
        let options = BlobPropertyBag::new();
        options.set_type("image/jpeg");
        let blob = Blob::new_with_u8_array_sequence_and_options(&parts, &options)
            .map_err(|_| "无法准备预览图像")?;
        let url = Url::create_object_url_with_blob(&blob).map_err(|_| "无法准备预览图像地址")?;
        Ok(Self {
            url,
            object_url: true,
        })
    }
    pub fn legacy(base64: &str) -> Self {
        Self {
            url: format!("data:image/jpeg;base64,{base64}"),
            object_url: false,
        }
    }
}
impl Drop for ImageSource {
    fn drop(&mut self) {
        if self.object_url {
            let _ = Url::revoke_object_url(&self.url);
        }
    }
}

pub async fn decode_image(
    image: HtmlImageElement,
    source: String,
    width: u32,
    height: u32,
) -> bool {
    if image.current_src() != source
        || wasm_bindgen_futures::JsFuture::from(image.decode())
            .await
            .is_err()
    {
        return false;
    }
    image.current_src() == source
        && image.natural_width() == width
        && image.natural_height() == height
}

pub fn now() -> f64 {
    web_sys::window()
        .and_then(|w| w.performance())
        .map(|p| p.now())
        .unwrap_or(f64::NAN)
}
pub fn hidden() -> bool {
    web_sys::window()
        .and_then(|w| w.document())
        .is_none_or(|d| d.hidden())
}
pub struct VisibilityListener {
    document: Document,
    callback: Closure<dyn FnMut(Event)>,
}
impl VisibilityListener {
    pub fn new(changed: Callback<bool>) -> Option<Self> {
        let document = web_sys::window()?.document()?;
        let callback = Closure::wrap(
            Box::new(move |_: Event| changed.emit(hidden())) as Box<dyn FnMut(Event)>
        );
        document
            .add_event_listener_with_callback("visibilitychange", callback.as_ref().unchecked_ref())
            .ok()?;
        Some(Self { document, callback })
    }
}
impl Drop for VisibilityListener {
    fn drop(&mut self) {
        let _ = self.document.remove_event_listener_with_callback(
            "visibilitychange",
            self.callback.as_ref().unchecked_ref(),
        );
    }
}

pub struct PaintOpportunity {
    id: i32,
    _callback: Closure<dyn FnMut(f64)>,
}
impl PaintOpportunity {
    pub fn new(done: Callback<()>) -> Option<Self> {
        let callback = Closure::wrap(Box::new(move |_: f64| done.emit(())) as Box<dyn FnMut(f64)>);
        let id = web_sys::window()?
            .request_animation_frame(callback.as_ref().unchecked_ref())
            .ok()?;
        Some(Self {
            id,
            _callback: callback,
        })
    }
}
impl Drop for PaintOpportunity {
    fn drop(&mut self) {
        if let Some(window) = web_sys::window() {
            let _ = window.cancel_animation_frame(self.id);
        }
    }
}
