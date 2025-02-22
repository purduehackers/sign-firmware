use esp_idf_svc::io::asynch::Write;
use http::Request;

use crate::{
    convert_error,
    net::{create_raw_request, generate_tls},
};

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub enum UnderlineMode {
    None,
    Single,
    Double,
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub enum JustifyMode {
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "type", content = "content")]
pub enum PrinterInstruction {
    Text(String),
    Image(String),
    Reverse(bool),
    Underline(UnderlineMode),
    Justify(JustifyMode),
    Strike(bool),
    Bold(bool),
    Italic(bool),
    PrintCut,
}

pub type PrinterMessage = Vec<PrinterInstruction>;

#[derive(Debug, Clone, Copy)]
pub enum PrinterEvent {
    NewBolt(u8),
    NewZap(u8),
    Zero,
    ButtonPressed,
}

impl PrinterEvent {
    fn message(&self) -> PrinterMessage {
        match self {
            PrinterEvent::Zero => vec![
                PrinterInstruction::Justify(JustifyMode::Center),
                PrinterInstruction::Bold(true),
                PrinterInstruction::Text("A NEW DAY DAWNS".to_string()),
                PrinterInstruction::PrintCut,
            ],
            PrinterEvent::NewBolt(b) => vec![
                PrinterInstruction::Text(format!("Lightning strikes! It is now bolt {b:x}")),
                PrinterInstruction::PrintCut,
            ],
            PrinterEvent::NewZap(z) => vec![
                PrinterInstruction::Text(format!(
                    "Did you just lick a 9V battery? It is now zap {z:x}"
                )),
                PrinterInstruction::PrintCut,
            ],
            PrinterEvent::ButtonPressed => vec![
                PrinterInstruction::Justify(JustifyMode::Center),
                PrinterInstruction::Bold(true),
                PrinterInstruction::Text("SOMEONE JUST PRESSED THE SECRET BUTTON!".to_string()),
                PrinterInstruction::PrintCut,
            ],
        }
    }
}

pub async fn post_event(event: PrinterEvent) -> anyhow::Result<()> {
    let url = "https://api.purduehackers.com/printer/print";
    let mut tls = generate_tls(url).await?;

    let data = serde_json::to_string(&event.message())?;

    let request = Request::builder()
        .method("POST")
        .header("User-Agent", "PHSign/1.0.0")
        .header("Content-Type", "application/json")
        .header("Host", "api.purduehackers.com")
        .header("Content-Length", data.len())
        .uri(url)
        .body(data)
        .unwrap();

    let request_text = create_raw_request(&request);

    tls.write_all(request_text.as_bytes())
        .await
        .map_err(convert_error)?;

    tls.flush().await?;

    Ok(())
}
