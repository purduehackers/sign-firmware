use crate::net::http;

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
    let data = serde_json::to_string(&event.message())?;

    http::http_post(
        "https://api.purduehackers.com/printer/print",
        &[("Content-Type", "application/json")],
        data.as_bytes(),
    )
    .await?;

    Ok(())
}
