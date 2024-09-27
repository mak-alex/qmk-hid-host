use crate::data_type::DataType;
use core_foundation::base::{CFRelease, TCFType};
use core_foundation::string::{CFString, CFStringRef};
use libc::c_void;
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, mpsc};
use std::thread;
use std::time::Duration;
use objc2::runtime::{AnyObject, NSObject, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::NSString;

use super::super::_base::Provider;
use std::collections::HashMap;

fn create_layout_map() -> HashMap<&'static str, &'static str> {
    let mut layout_map = HashMap::new();
    layout_map.insert("U.S.", "en");
    layout_map.insert("ABC", "en");
    layout_map.insert("Russian", "ru");
    tracing::debug!("Layout map created with keys: {:?}", layout_map.keys());
    layout_map
}

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn TISCopyCurrentKeyboardLayoutInputSource() -> *mut c_void;
    fn TISGetInputSourceProperty(input_source: *mut c_void, key: CFStringRef) -> *mut CFStringRef;
}

fn get_keyboard_layout() -> Option<String> {
    unsafe {
        let layout_input_source = TISCopyCurrentKeyboardLayoutInputSource();
        if layout_input_source.is_null() {
            tracing::error!("Failed to get keyboard layout input source");
            return None;
        }

        let k_tis_property_input_source_id = CFString::from_static_string("TISPropertyInputSourceID");
        let layout_id_ptr = TISGetInputSourceProperty(layout_input_source, k_tis_property_input_source_id.as_concrete_TypeRef());
        CFRelease(layout_input_source);

        if layout_id_ptr.is_null() {
            tracing::error!("Failed to get input source property");
            return None;
        }

        let layout_id = layout_id_ptr as CFStringRef;
        if layout_id.is_null() {
            tracing::error!("Layout ID is null");
            return None;
        }

        let layout_string = CFString::wrap_under_get_rule(layout_id).to_string();
        tracing::debug!("Detected keyboard layout: {}", layout_string);
        Some(layout_string)
    }
}

// Функция для регистрации уведомлений через DistributedNotificationCenter
fn register_for_layout_change_notifications() {
    unsafe {
        let notification_center: *mut AnyObject = msg_send![class!(NSDistributedNotificationCenter), defaultCenter];
        let layout_change_name = NSString::from_str("com.apple.inputSourceChanged");

        // Создаем экземпляр объекта NSObject
        let observer: *mut NSObject = msg_send![class!(NSObject), new];

        let _: () = msg_send![notification_center,
            addObserver: observer,
            selector: sel!(handleLayoutChange:),
            name: layout_change_name.as_ref(),  // Используем публичный метод as_raw()
            object: std::ptr::null_mut::<AnyObject>()
        ];
    }
}

// Колбэк для обработки изменений раскладки клавиатуры
extern "C" fn handle_layout_change(_: &AnyObject, _: Sel) {
    tracing::info!("Keyboard layout changed!");
}

fn extract_layout_name(full_layout: &str) -> Option<String> {
    tracing::debug!("Extracting layout name from: {}", full_layout);

    if full_layout.starts_with("com.apple.keylayout.") {
        let short_layout = full_layout.trim_start_matches("com.apple.keylayout.").to_string();
        tracing::debug!("Extracted layout name: {}", short_layout);
        Some(short_layout)
    } else {
        tracing::warn!("Unknown layout format: {}", full_layout);
        None
    }
}

fn get_keyboard_layout_code(layout: &str, layout_map: &HashMap<&'static str, &'static str>) -> Option<String> {
    tracing::debug!("Looking up layout code for: {}", layout);

    if let Some(extracted_layout) = extract_layout_name(layout) {
        let layout_code = layout_map.get(extracted_layout.as_str()).map(|&code| code.to_string());
        tracing::debug!("Mapped layout code: {:?}", layout_code);
        layout_code
    } else {
        tracing::warn!("Failed to extract layout name from: {}", layout);
        None
    }
}

fn send_data(value: &String, layouts: &Vec<String>, data_sender: &mpsc::Sender<Vec<u8>>) {
    tracing::info!("Sending layout data: '{0}', layout list: {1:?}", value, layouts);

    if let Some(index) = layouts.iter().position(|r| r == value) {
        let data = vec![DataType::Layout as u8, index as u8];
        if let Err(e) = data_sender.try_send(data) {
            tracing::error!("Failed to send layout data: {}", e);
        }
    } else {
        tracing::warn!("Layout not found in the predefined list: {}", value);
    }
}

pub struct LayoutProvider {
    data_sender: mpsc::Sender<Vec<u8>>,
    connected_sender: broadcast::Sender<bool>,
    layouts: Vec<String>,
}

impl LayoutProvider {
    pub fn new(data_sender: mpsc::Sender<Vec<u8>>, connected_sender: broadcast::Sender<bool>, layouts: Vec<String>) -> Box<dyn Provider> {
        let provider = LayoutProvider {
            data_sender,
            connected_sender,
            layouts,
        };
        Box::new(provider)
    }
}

impl Provider for LayoutProvider {
    fn start(&self) {
        tracing::info!("Layout Provider started");

        let data_sender = self.data_sender.clone();
        let layouts = self.layouts.clone();
        let connected_sender = self.connected_sender.clone();
        let layout_map = create_layout_map(); // Создаём маппинг для раскладок
        let mut synced_layout = "".to_string();

        let is_connected = Arc::new(Mutex::new(true));
        let is_connected_ref = is_connected.clone();

        // Запускаем провайдера в отдельном потоке
        std::thread::spawn(move || {
            // Поток для отслеживания подключения/отключения
            let mut connected_receiver = connected_sender.subscribe();
            std::thread::spawn(move || {
                loop {
                    if !connected_receiver.try_recv().unwrap_or(true) {
                        let mut is_connected = is_connected_ref.lock().unwrap();
                        *is_connected = false;
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            });

            // Основной цикл для проверки раскладки клавиатуры
            loop {
                if !*(is_connected.lock().unwrap()) {
                    break;
                }

                // Получаем текущую раскладку
                if let Some(layout) = get_keyboard_layout() {
                    if let Some(layout_code) = get_keyboard_layout_code(&layout, &layout_map) {
                        if synced_layout != layout_code {
                            synced_layout = layout_code.clone();
                            send_data(&synced_layout, &layouts, &data_sender);
                        }
                    } else {
                        tracing::warn!("Unknown layout: {}", layout);
                    }
                }

                // Ожидание перед следующей проверкой
                thread::sleep(Duration::from_millis(500)); // Опрос каждые 500 мс
            }

            tracing::info!("Layout Provider stopped");
        });
    }
}
