//! Everything that talks to the phone, and nothing else.
//!
//! No layout rules live here — this module only knows lockdown, SpringBoard and
//! how to turn what they answer into JSON.

use base64::Engine as _;
use idevice::provider::UsbmuxdProvider;
use idevice::services::lockdown::LockdownClient;
use idevice::services::springboardservices::SpringBoardServicesClient;
use idevice::usbmuxd::{Connection, UsbmuxdAddr, UsbmuxdConnection};
use idevice::IdeviceService;
use serde::Serialize;
use std::fmt::Display;
use std::future::Future;
use std::time::Duration;

const FORMAT_VERSION: &str = "2";
const LABEL: &str = "tidygrid";
const IO_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
const PLIST_TAG: &str = "$tidygrid.plist";

pub type Result<T> = std::result::Result<T, String>;

#[derive(Debug, Serialize)]
pub struct DeviceRow {
    pub serial: String,
    pub connection: String,
    pub device_id: u32,
}

#[derive(Debug, Default, Serialize)]
pub struct DeviceInfo {
    pub name: Option<String>,
    pub ios: Option<String>,
    pub model: Option<String>,
    pub serial: Option<String>,
}

async fn within<T, E>(
    what: &str,
    future: impl Future<Output = std::result::Result<T, E>>,
) -> Result<T>
where
    E: Display,
{
    tokio::time::timeout(IO_TIMEOUT, future)
        .await
        .map_err(|_| format!("timed out while {what}"))?
        .map_err(|error| format!("{what}: {error}"))
}

fn tagged(kind: &str, value: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ PLIST_TAG: { "type": kind, "value": value } })
}

/// SpringBoard answers in plist types. Types JSON cannot represent natively
/// use a reserved tagged object so snapshots remain lossless and editable.
fn to_json(value: &plist::Value) -> Result<serde_json::Value> {
    match value {
        plist::Value::String(text) => Ok(serde_json::Value::String(text.clone())),
        plist::Value::Boolean(flag) => Ok(serde_json::Value::Bool(*flag)),
        plist::Value::Integer(number) => number
            .as_signed()
            .map(serde_json::Value::from)
            .or_else(|| number.as_unsigned().map(serde_json::Value::from))
            .ok_or_else(|| "SpringBoard returned an unsupported integer".to_string()),
        plist::Value::Real(number) => Ok(serde_json::Number::from_f64(*number)
            .map(serde_json::Value::Number)
            .ok_or_else(|| "SpringBoard returned a non-finite number".to_string())?),
        plist::Value::Array(items) => Ok(serde_json::Value::Array(
            items.iter().map(to_json).collect::<Result<Vec<_>>>()?,
        )),
        plist::Value::Dictionary(map) => Ok(serde_json::Value::Object(
            map.iter()
                .map(|(key, item)| Ok((key.clone(), to_json(item)?)))
                .collect::<Result<serde_json::Map<_, _>>>()?,
        )),
        plist::Value::Data(bytes) => Ok(tagged(
            "data",
            serde_json::Value::String(base64::engine::general_purpose::STANDARD.encode(bytes)),
        )),
        plist::Value::Date(date) => Ok(tagged(
            "date",
            serde_json::Value::String(date.to_xml_format()),
        )),
        plist::Value::Uid(uid) => Ok(tagged("uid", serde_json::Value::from(uid.get()))),
        _ => Err("SpringBoard returned an unsupported plist value".to_string()),
    }
}

fn to_plist(value: &serde_json::Value) -> Result<plist::Value> {
    match value {
        serde_json::Value::Bool(flag) => Ok(plist::Value::Boolean(*flag)),
        serde_json::Value::String(text) => Ok(plist::Value::String(text.clone())),
        serde_json::Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                Ok(plist::Value::from(value))
            } else if let Some(value) = number.as_u64() {
                Ok(plist::Value::from(value))
            } else if let Some(value) = number.as_f64() {
                Ok(plist::Value::from(value))
            } else {
                Err("layout contains an unsupported number".to_string())
            }
        }
        serde_json::Value::Array(items) => Ok(plist::Value::Array(
            items.iter().map(to_plist).collect::<Result<Vec<_>>>()?,
        )),
        serde_json::Value::Object(map) => {
            if map.len() == 1 {
                if let Some(tag) = map.get(PLIST_TAG).and_then(serde_json::Value::as_object) {
                    let kind = tag.get("type").and_then(serde_json::Value::as_str);
                    let value = tag.get("value");
                    return match (kind, value) {
                        (Some("data"), Some(serde_json::Value::String(encoded))) => {
                            let bytes = base64::engine::general_purpose::STANDARD
                                .decode(encoded)
                                .map_err(|_| "layout contains invalid tagged plist data")?;
                            Ok(plist::Value::Data(bytes))
                        }
                        (Some("date"), Some(serde_json::Value::String(date))) => {
                            let date = plist::Date::from_xml_format(date)
                                .map_err(|_| "layout contains an invalid tagged plist date")?;
                            Ok(plist::Value::Date(date))
                        }
                        (Some("uid"), Some(value)) => value
                            .as_u64()
                            .map(plist::Uid::new)
                            .map(plist::Value::Uid)
                            .ok_or_else(|| "layout contains an invalid tagged plist UID".into()),
                        _ => Err("layout contains an invalid tagged plist value".into()),
                    };
                }
            }
            Ok(plist::Value::Dictionary(
                map.iter()
                    .map(|(key, item)| Ok((key.clone(), to_plist(item)?)))
                    .collect::<Result<plist::Dictionary>>()?,
            ))
        }
        serde_json::Value::Null => Err("layout contains null, which is not a plist value".into()),
    }
}

/// SpringBoard reports modification timestamps as plist dates, while the
/// previously working IconState implementation writes them back as strings.
/// Sending the dates back as `<date>` values was silently ignored on the
/// tested iOS version, so normalize only the outbound icon-state payload while
/// keeping snapshots lossless.
fn icon_state_plist(value: &serde_json::Value) -> Result<plist::Value> {
    fn stringify_dates(value: &mut plist::Value) {
        match value {
            plist::Value::Date(date) => {
                *value = plist::Value::String(date.to_xml_format());
            }
            plist::Value::Array(items) => {
                for item in items {
                    stringify_dates(item);
                }
            }
            plist::Value::Dictionary(items) => {
                for item in items.values_mut() {
                    stringify_dates(item);
                }
            }
            _ => {}
        }
    }

    let mut value = to_plist(value)?;
    stringify_dates(&mut value);
    Ok(value)
}

fn command(name: &str) -> plist::Dictionary {
    let mut request = plist::Dictionary::new();
    request.insert("command".into(), plist::Value::String(name.into()));
    request
}

async fn muxer() -> Result<UsbmuxdConnection> {
    within("waiting for usbmuxd", UsbmuxdConnection::default()).await
}

pub async fn devices() -> Result<Vec<DeviceRow>> {
    let found = within("listing devices", muxer().await?.get_devices()).await?;

    let mut devices: Vec<DeviceRow> = found
        .into_iter()
        .map(|device| DeviceRow {
            serial: device.udid,
            connection: match device.connection_type {
                Connection::Usb => "USB".into(),
                Connection::Network(address) => address.to_string(),
                Connection::Unknown(what) => what,
            },
            device_id: device.device_id,
        })
        .collect();
    devices.sort_by(|left, right| left.serial.cmp(&right.serial));
    Ok(devices)
}

async fn provider(serial: Option<&str>) -> Result<(UsbmuxdProvider, String)> {
    let mut muxer = muxer().await?;
    let found = within("listing devices", muxer.get_devices()).await?;

    let mut matching: Vec<_> = found
        .into_iter()
        .filter(|device| serial.is_none_or(|serial| device.udid == serial))
        .filter(|device| matches!(device.connection_type, Connection::Usb))
        .collect();

    if matching.is_empty() {
        return Err("no matching iPhone is connected over USB".to_string());
    }
    if serial.is_none() && matching.len() > 1 {
        return Err("more than one iPhone is connected over USB; pass --device UDID".to_string());
    }
    let device = matching.remove(0);
    let resolved = device.udid.clone();

    let addr = UsbmuxdAddr::from_env_var().unwrap_or_default();
    Ok((device.to_provider(addr, LABEL), resolved))
}

pub async fn resolve_device(serial: Option<&str>) -> Result<String> {
    provider(serial).await.map(|(_, resolved)| resolved)
}

pub async fn info(provider: &UsbmuxdProvider) -> Result<DeviceInfo> {
    let mut lockdown = within(
        "connecting to the device over USB",
        LockdownClient::connect(provider),
    )
    .await?;

    let values = within(
        "reading the device identity",
        lockdown.get_value(None, None),
    )
    .await?;

    let read = |key: &str| {
        values
            .as_dictionary()
            .and_then(|map| map.get(key))
            .and_then(plist::Value::as_string)
            .map(str::to_owned)
    };

    Ok(DeviceInfo {
        name: read("DeviceName"),
        ios: read("ProductVersion"),
        model: read("ProductType"),
        serial: read("UniqueDeviceID"),
    })
}

async fn springboard(provider: &UsbmuxdProvider) -> Result<SpringBoardServicesClient> {
    within(
        "opening SpringBoard services",
        SpringBoardServicesClient::connect(provider),
    )
    .await
}

/// SpringBoard frames every message with a big-endian length. The crate wraps
/// this in private methods, so the framing is done here against its public raw
/// socket — which also keeps the commands we send in one place, rather than
/// spread between our code and the crate's.
async fn tell(
    client: &mut SpringBoardServicesClient,
    request: plist::Dictionary,
    what: &str,
) -> Result<()> {
    let mut body = Vec::new();
    plist::Value::Dictionary(request)
        .to_writer_xml(&mut body)
        .map_err(|error| format!("could not build the request for {what}: {error}"))?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(format!(
            "the request for {what} is too large ({} bytes; limit is {MAX_FRAME_BYTES})",
            body.len()
        ));
    }

    let length =
        u32::try_from(body.len()).map_err(|_| format!("the request for {what} is too large"))?;
    let mut message = length.to_be_bytes().to_vec();
    message.extend_from_slice(&body);

    within(
        &format!("sending the request for {what}"),
        client.idevice.send_raw(&message),
    )
    .await
}

async fn ask(
    client: &mut SpringBoardServicesClient,
    request: plist::Dictionary,
    what: &str,
) -> Result<plist::Value> {
    tell(client, request, what).await?;

    let head = within(
        &format!("reading the response header for {what}"),
        client.idevice.read_raw(4),
    )
    .await?;
    let length = u32::from_be_bytes(
        head.as_slice()
            .try_into()
            .map_err(|_| format!("{what} came back truncated"))?,
    ) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(format!(
            "{what} response is too large ({length} bytes; limit is {MAX_FRAME_BYTES})"
        ));
    }

    let body = within(
        &format!("reading the response body for {what}"),
        client.idevice.read_raw(length),
    )
    .await?;

    plist::from_bytes(&body).map_err(|error| format!("{what} came back malformed: {error}"))
}

async fn read_state_as(
    client: &mut SpringBoardServicesClient,
    format: Option<&str>,
) -> Result<serde_json::Value> {
    let mut request = command("getIconState");
    if let Some(format) = format {
        request.insert("formatVersion".into(), plist::Value::String(format.into()));
    }
    let state = to_json(&ask(client, request, "the home screen").await?)?;
    if !state.is_array() {
        return Err("SpringBoard returned a malformed Home Screen layout".to_string());
    }
    Ok(state)
}

async fn read_state(client: &mut SpringBoardServicesClient) -> Result<serde_json::Value> {
    read_state_as(client, Some(FORMAT_VERSION)).await
}

async fn read_metrics(client: &mut SpringBoardServicesClient) -> Result<serde_json::Value> {
    let metrics = to_json(
        &ask(
            client,
            command("getHomeScreenIconMetrics"),
            "the home screen grid",
        )
        .await?,
    )?;
    if !metrics.is_object() {
        return Err("SpringBoard returned malformed Home Screen metrics".to_string());
    }
    Ok(metrics)
}

/// The layout and the device's own grid, over one connection.
pub async fn icon_state(
    serial: Option<&str>,
) -> Result<(DeviceInfo, String, serde_json::Value, serde_json::Value)> {
    let (provider, resolved) = provider(serial).await?;
    let about = info(&provider).await?;
    let mut client = springboard(&provider).await?;

    let state = read_state(&mut client).await?;
    let metrics = read_metrics(&mut client).await?;
    Ok((about, resolved, state, metrics))
}

/// Send a layout and read back what SpringBoard actually settled on.
///
/// setIconState answers nothing and the socket is spent once it lands, so the
/// read back has to happen over a connection of its own.
pub async fn write_icon_state(
    serial: &str,
    state: &serde_json::Value,
) -> Result<(DeviceInfo, serde_json::Value)> {
    let (provider, resolved) = provider(Some(serial)).await?;
    if resolved != serial {
        return Err("the selected iPhone identity changed".to_string());
    }
    let about = info(&provider).await?;

    let mut request = command("setIconState");
    request.insert("iconState".into(), icon_state_plist(state)?);
    tell(&mut springboard(&provider).await?, request, "the write").await?;

    let settled = read_state(&mut springboard(&provider).await?).await?;
    Ok((about, settled))
}

#[cfg(test)]
mod tests {
    use super::{icon_state_plist, to_json, to_plist};

    #[test]
    fn plist_only_types_round_trip_through_tagged_json() {
        let values = [
            plist::Value::Data(vec![0, 1, 2, 255]),
            plist::Value::Date(
                plist::Date::from_xml_format("2026-09-16T12:34:56.123456Z").unwrap(),
            ),
            plist::Value::Uid(plist::Uid::new(u64::MAX)),
        ];
        for value in values {
            assert_eq!(to_plist(&to_json(&value).unwrap()).unwrap(), value);
        }
    }

    #[test]
    fn icon_state_dates_are_encoded_as_strings_for_springboard() {
        let date = plist::Date::from_xml_format("2026-09-16T12:34:56.123456Z").unwrap();
        let tagged = to_json(&plist::Value::Dictionary(plist::Dictionary::from_iter([
            (String::from("iconModDate"), plist::Value::Date(date)),
            (
                String::from("displayName"),
                plist::Value::String("Example".into()),
            ),
        ])))
        .unwrap();

        let encoded = icon_state_plist(&tagged).unwrap();
        let dictionary = encoded.as_dictionary().unwrap();
        assert!(dictionary["iconModDate"].as_string().is_some());
        assert_eq!(dictionary["displayName"].as_string(), Some("Example"));
    }

    #[test]
    fn icon_state_compatibility_conversion_is_recursive_and_preserves_other_types() {
        let date = plist::Date::from_xml_format("2026-09-16T12:34:56.123456Z").unwrap();
        let state = plist::Value::Array(vec![plist::Value::Dictionary(
            plist::Dictionary::from_iter([(
                String::from("iconLists"),
                plist::Value::Array(vec![plist::Value::Array(vec![plist::Value::Dictionary(
                    plist::Dictionary::from_iter([
                        (String::from("iconModDate"), plist::Value::Date(date)),
                        (String::from("iconImage"), plist::Value::Data(vec![0, 1, 2])),
                        (
                            String::from("iconUid"),
                            plist::Value::Uid(plist::Uid::new(7)),
                        ),
                    ]),
                )])]),
            )]),
        )]);

        let tagged = to_json(&state).unwrap();
        let encoded = icon_state_plist(&tagged).unwrap();
        let icon = encoded.as_array().unwrap()[0].as_dictionary().unwrap()["iconLists"]
            .as_array()
            .unwrap()[0]
            .as_array()
            .unwrap()[0]
            .as_dictionary()
            .unwrap();

        assert!(icon["iconModDate"].as_string().is_some());
        assert_eq!(icon["iconImage"].as_data(), Some([0, 1, 2].as_slice()));
        assert_eq!(icon["iconUid"].as_uid().map(|uid| uid.get()), Some(7));
    }

    #[test]
    fn malformed_tags_and_null_are_rejected() {
        assert!(
            to_plist(&serde_json::json!({"$tidygrid.plist": {"type": "uid", "value": -1}}))
                .is_err()
        );
        assert!(to_plist(&serde_json::Value::Null).is_err());
    }
}
