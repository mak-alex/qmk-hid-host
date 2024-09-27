use objc2::runtime::AnyObject;
use objc2::rc::{autoreleasepool, AutoreleasePool, Retained};
use objc2::{msg_send, ClassType};
use objc2_foundation::{ns_string, NSString, NSDictionary};
use objc2_media_player::MPNowPlayingInfoCenter;
use tokio::sync::{broadcast, mpsc};
use crate::data_type::DataType;
use super::super::_base::Provider;
use std::sync::atomic::{AtomicBool, Ordering};
use translit::{Transliterator, CharsMapping};

// Определяем таблицу для транслитерации русского алфавита в латиницу
fn get_transliteration_table() -> CharsMapping {
    [
        // Строчные буквы
        ("а", "a"), ("б", "b"), ("в", "v"), ("г", "g"), ("д", "d"),
        ("е", "e"), ("ё", "yo"), ("ж", "zh"), ("з", "z"), ("и", "i"),
        ("й", "y"), ("к", "k"), ("л", "l"), ("м", "m"), ("н", "n"),
        ("о", "o"), ("п", "p"), ("р", "r"), ("с", "s"), ("т", "t"),
        ("у", "u"), ("ф", "f"), ("х", "kh"), ("ц", "ts"), ("ч", "ch"),
        ("ш", "sh"), ("щ", "sch"), ("ъ", ""), ("ы", "y"), ("ь", ""),
        ("э", "e"), ("ю", "yu"), ("я", "ya"),
        // Заглавные буквы
        ("А", "A"), ("Б", "B"), ("В", "V"), ("Г", "G"), ("Д", "D"),
        ("Е", "E"), ("Ё", "Yo"), ("Ж", "Zh"), ("З", "Z"), ("И", "I"),
        ("Й", "Y"), ("К", "K"), ("Л", "L"), ("М", "M"), ("Н", "N"),
        ("О", "O"), ("П", "P"), ("Р", "R"), ("С", "S"), ("Т", "T"),
        ("У", "U"), ("Ф", "F"), ("Х", "Kh"), ("Ц", "Ts"), ("Ч", "Ch"),
        ("Ш", "Sh"), ("Щ", "Sch"), ("Ъ", ""), ("Ы", "Y"), ("Ь", ""),
        ("Э", "E"), ("Ю", "Yu"), ("Я", "Ya")
    ].iter().cloned().collect()
}


fn transliterate_text(text: &str) -> String {
    let table = get_transliteration_table();  // Получаем таблицу транслитерации
    let transliterator = Transliterator::new(table);  // Создаем объект Transliterator с маппингом
    let result = transliterator.convert(text, false);  // Применяем транслитерацию
    result
}
// Для отслеживания переключения между методами
static USE_APPLE_SCRIPT: AtomicBool = AtomicBool::new(false);

// Функция для выполнения AppleScript через Scripting Bridge
fn execute_applescript(script: &str) -> Option<String> {
    use std::process::Command;
    tracing::debug!("Executing AppleScript: {}", script);  // Лог выполнения AppleScript
    match Command::new("osascript").arg("-e").arg(script).output() {
        Ok(output) if output.status.success() => {
            let result = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if result.is_empty() {
                tracing::warn!("AppleScript returned an empty result.");
                None
            } else {
                Some(result)
            }
        },
        Ok(output) => {
            tracing::error!("AppleScript failed with status: {}. Stderr: {}", output.status, String::from_utf8_lossy(&output.stderr));
            None
        },
        Err(e) => {
            tracing::error!("Failed to execute AppleScript: {}", e);
            None
        }
    }
}


// Получаем информацию о текущем треке через AppleScript
fn get_now_playing_via_applescript() -> Option<(String, String)> {
    let script = r#"
        tell application "Spotify"
            if it is running then
                set artist_name to artist of current track
                set track_name to name of current track
                return artist_name & "|" & track_name
            end if
        end tell

        tell application "Music"
            if it is running then
                set artist_name to artist of current track
                set track_name to name of current track
                return artist_name & "|" & track_name
            end if
        end tell

        return ""
    "#;

    if let Some(result) = execute_applescript(script) {
        if !result.is_empty() {
            let parts: Vec<&str> = result.split('|').collect();
            if parts.len() == 2 {
                tracing::debug!("AppleScript retrieved info: {} - {}", parts[0], parts[1]);
                return Some((parts[0].to_string(), parts[1].to_string()));
            }
        }
    }

    None
}

unsafe fn get_now_playing_info() -> Option<Retained<NSDictionary<NSString, AnyObject>>> {
    let info_center = MPNowPlayingInfoCenter::defaultCenter();
    let playback_state: u64 = msg_send![&info_center, playbackState];
    tracing::info!("MPNowPlayingInfoCenter playback state: {}", playback_state);

    let now_playing_info = info_center.nowPlayingInfo();

    if let Some(ref info) = now_playing_info {
        tracing::info!("Now playing info dictionary found: {:?}", info);
    } else {
        tracing::warn!("Failed to retrieve now playing info or info is empty.");
    }

    now_playing_info
}

unsafe fn get_media_data(info: &NSDictionary<NSString, AnyObject>, pool: AutoreleasePool<'_>) -> (Option<String>, Option<String>) {
    let artist_key = ns_string!("MPMediaItemPropertyArtist");
    let title_key = ns_string!("MPMediaItemPropertyTitle");

    let artist = info
        .get(&*artist_key)
        .and_then(|obj| {
            let is_nsstring: bool = msg_send![obj, isKindOfClass: NSString::class()];
            if is_nsstring {
                Some(&*(obj as *const AnyObject as *const NSString))
            } else {
                None
            }
        })
        .map(|a| a.as_str(pool).to_owned());

    let title = info
        .get(&*title_key)
        .and_then(|obj| {
            let is_nsstring: bool = msg_send![obj, isKindOfClass: NSString::class()];
            if is_nsstring {
                Some(&*(obj as *const AnyObject as *const NSString))
            } else {
                None
            }
        })
        .map(|t| t.as_str(pool).to_owned());

    tracing::info!("Artist: {:?}, Title: {:?}", artist, title);

    (artist, title)
}

fn send_media_data(artist: &Option<String>, title: &Option<String>, data_sender: &mpsc::Sender<Vec<u8>>, last_artist: &mut String, last_title: &mut String) {
    if let Some(new_artist) = artist {
        let artist_transliterated = transliterate_text(new_artist);  // Применяем транслитерацию
        if artist_transliterated != *last_artist {
            tracing::info!("Sending new artist (transliterated): {}", artist_transliterated);
            send_data(DataType::MediaArtist, &artist_transliterated, data_sender);
            *last_artist = artist_transliterated;
        }
    }

    if let Some(new_title) = title {
        let title_transliterated = transliterate_text(new_title);  // Применяем транслитерацию
        if title_transliterated != *last_title {
            tracing::info!("Sending new title (transliterated): {}", title_transliterated);
            send_data(DataType::MediaTitle, &title_transliterated, data_sender);
            *last_title = title_transliterated;
        }
    }
}


fn send_data(data_type: DataType, value: &str, data_sender: &mpsc::Sender<Vec<u8>>) {
    let mut data = value.as_bytes().to_vec();
    // data.truncate(30);
    data.insert(0, data.len() as u8);
    data.insert(0, data_type as u8);

    tracing::info!("Sending data: {:?}", data);

    match data_sender.try_send(data) {
        Ok(_) => tracing::info!("Data sent successfully."),
        Err(e) => tracing::error!("Failed to send data: {}", e),
    }
}

pub struct MediaProvider {
    data_sender: mpsc::Sender<Vec<u8>>,
    connected_sender: broadcast::Sender<bool>,
}

impl MediaProvider {
    pub fn new(data_sender: mpsc::Sender<Vec<u8>>, connected_sender: broadcast::Sender<bool>) -> Box<dyn Provider> {
        tracing::info!("MediaProvider is being initialized.");

        let provider = MediaProvider {
            data_sender,
            connected_sender,
        };
        Box::new(provider)
    }
}

impl Provider for MediaProvider {
    fn start(&self) {
        tracing::info!("Starting MediaProvider...");
        let data_sender = self.data_sender.clone();
        let connected_sender = self.connected_sender.clone();

        std::thread::spawn(move || {
            tracing::debug!("Media Provider started thread.");

            let mut connected_receiver = connected_sender.subscribe();
            let mut last_artist = String::new();
            let mut last_title = String::new();

            loop {
                if !connected_receiver.try_recv().unwrap_or(true) {
                    tracing::info!("Disconnected from sender.");
                    break;
                }

                if USE_APPLE_SCRIPT.load(Ordering::Relaxed) {
                    if let Some((artist, title)) = get_now_playing_via_applescript() {
                        tracing::debug!("AppleScript retrieved info: {} - {}", artist, title);
                        send_media_data(&Some(artist), &Some(title), &data_sender, &mut last_artist, &mut last_title);
                    } else {
                        tracing::warn!("AppleScript failed, retrying after delay.");
                        std::thread::sleep(std::time::Duration::from_secs(2));  // Добавляем небольшую задержку перед повтором
                    }
                } else {
                    autoreleasepool(|pool| unsafe {
                        if let Some(info) = get_now_playing_info() {
                            let (artist, title) = get_media_data(&info, pool);
                            if artist.is_none() || title.is_none() {
                                tracing::warn!("MPNowPlayingInfoCenter incomplete, switching to AppleScript.");
                                USE_APPLE_SCRIPT.store(true, Ordering::Relaxed);
                            } else {
                                // Принудительно обновляем и артиста, и трек вместе
                                if artist.is_some() && title.is_some() {
                                    send_media_data(&artist, &title, &data_sender, &mut last_artist, &mut last_title);
                                } else {
                                    tracing::warn!("Incomplete media info (missing artist or title). Retrying...");
                                }
                            }
                        } else {
                            tracing::warn!("No info from MPNowPlayingInfoCenter, switching to AppleScript.");
                            USE_APPLE_SCRIPT.store(true, Ordering::Relaxed);
                        }
                    });
                }

                // Увеличиваем задержку, чтобы убедиться, что данные собираются корректно
                std::thread::sleep(std::time::Duration::from_secs(2));
            }

            tracing::info!("Media Provider stopped");
        });
    }
}

