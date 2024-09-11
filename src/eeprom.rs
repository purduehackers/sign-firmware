// use super::ceil;
use core::mem::size_of;
use embassy_time::{Duration, Timer};
use esp_idf_svc::hal::spi::{SpiDeviceDriver, SpiDriver};
use log::info;

const READ_INSTRUCTION: u8 = 0b0000_0011;
const WRITE_INSTRUCTION: u8 = 0b0000_0010;
const ERASE_INSTRUCTION: u8 = 0b1100_0111;
const WRITE_ENABLE_INSTRUCTION: u8 = 0b0000_0110;
const READ_STATUS_INSTRUCTION: u8 = 0b0000_0101;
const WRITE_STATUS_INSTRUCTION: u8 = 0b0000_0001;

/// 0xdeadbeef 0x8acc8acc (deadbeef hacchacc)
const READ_WRITE_TEST: [u8; 8] = [0xde, 0xad, 0xbe, 0xef, 0x8a, 0xcc, 0x8a, 0xcc];
const METADATA_BLOCK_START: u32 = 0x10;
const DATA_BLOCK_START: u32 = 0x100;
const CONFIG_VERSION: u8 = 1;

const DELAY: Duration = Duration::from_millis(10);

pub struct Eeprom<'a> {
    spi: SpiDeviceDriver<'a, SpiDriver<'a>>,
    pub metadata: Metadata,
}

#[derive(bincode::Decode, bincode::Encode)]
pub struct Metadata {
    config_version: u8,
    pub written_configs: u8,
}

impl Default for Metadata {
    fn default() -> Self {
        Self {
            config_version: CONFIG_VERSION,
            written_configs: 0,
        }
    }
}

const BINCODE_CONFIG: bincode::config::Configuration<bincode::config::BigEndian> =
    bincode::config::standard()
        .with_big_endian()
        .with_variable_int_encoding();

impl<'a> Eeprom<'a> {
    pub async fn from_spi(spi: SpiDeviceDriver<'a, SpiDriver<'a>>) -> anyhow::Result<Self> {
        let mut eeprom = Self {
            spi,
            metadata: Default::default(),
        };

        eeprom.write_status(0).await?;

        info!(
            "Read status register: 0b{:08b}",
            eeprom.read_status().await?
        );

        eeprom.assert_read_write_works().await?;

        let mut metadata = [0x0; size_of::<Metadata>()];

        eeprom
            .read_bytes(METADATA_BLOCK_START, &mut metadata)
            .await?;

        let (metadata, _) = bincode::decode_from_slice::<Metadata, _>(&metadata, BINCODE_CONFIG)
            .expect("decode metadata");

        if metadata.config_version != CONFIG_VERSION {
            eeprom.erase().await?;
            eeprom.write_config().await?;
        } else {
            eeprom.metadata = metadata;
        }

        Ok(eeprom)
    }

    async fn write_config(&mut self) -> anyhow::Result<()> {
        let mut metadata = [0x0_u8; size_of::<Metadata>()];

        bincode::encode_into_slice(&self.metadata, &mut metadata, BINCODE_CONFIG)
            .expect("encode metadata");

        self.write_bytes(METADATA_BLOCK_START, &metadata).await
    }

    pub async fn read_bytes(&mut self, address: u32, buffer: &mut [u8]) -> anyhow::Result<()> {
        self.spi.write(&READ_INSTRUCTION.to_be_bytes())?;
        self.spi.write(&address.to_be_bytes()[1..])?;

        self.spi.read(buffer)?;

        Timer::after(DELAY).await;

        Ok(())
    }

    pub async fn write_bytes(&mut self, address: u32, buffer: &[u8]) -> anyhow::Result<()> {
        // Write to the next block
        let diff = 256 - (address % 256);
        if buffer.len() < diff as usize {
            return self.write_some_bytes(address, buffer).await;
        }
        self.write_some_bytes(address, &buffer[..diff as usize])
            .await?;
        let address = address + diff;
        let buffer = &buffer[diff as usize..];

        // Write next blocks
        for offset_32 in 0..(buffer.len() as f32 / 256.0).ceil() as u32 {
            let offset = offset_32 as usize;
            self.write_some_bytes(
                address + offset_32 * 256,
                buffer
                    .get(offset * 256..(offset + 1) * 256)
                    .unwrap_or(buffer.get(offset * 256..).expect("valid bounds")),
            )
            .await?;
        }

        Ok(())
    }

    async fn write_some_bytes(&mut self, address: u32, buffer: &[u8]) -> anyhow::Result<()> {
        assert!(
            buffer.len() as u32 <= 256 - (address % 256),
            "Write buffer overflow for address 0x{address:x} and size {}!",
            buffer.len()
        );
        //self.cs.set_high();
        self.enable_write().await?;

        self.spi.write(&WRITE_INSTRUCTION.to_be_bytes())?;
        self.spi.write(&address.to_be_bytes()[1..])?;

        self.spi.write(buffer)?;

        Timer::after(DELAY).await;

        Ok(())
    }

    async fn enable_write(&mut self) -> anyhow::Result<()> {
        self.spi.write(&WRITE_ENABLE_INSTRUCTION.to_be_bytes())?;

        Timer::after(DELAY).await;
        Ok(())
    }

    async fn erase(&mut self) -> anyhow::Result<()> {
        self.spi.write(&ERASE_INSTRUCTION.to_be_bytes())?;

        Timer::after(DELAY).await;
        Ok(())
    }

    async fn read_status(&mut self) -> anyhow::Result<u8> {
        self.spi.write(&READ_STATUS_INSTRUCTION.to_be_bytes())?;
        let mut data = [0x0; 1];
        self.spi.read(&mut data)?;
        Ok(data[0])
    }

    async fn write_status(&mut self, status: u8) -> anyhow::Result<()> {
        self.spi.write(&WRITE_STATUS_INSTRUCTION.to_be_bytes())?;
        self.spi.write(&[status])?;
        Timer::after(DELAY).await;
        Ok(())
    }

    pub async fn assert_read_write_works(&mut self) -> anyhow::Result<()> {
        self.write_bytes(0x0, &READ_WRITE_TEST).await?;

        let mut buffer = [0x0; 8];

        self.read_bytes(0x0, &mut buffer).await?;

        assert_eq!(buffer, READ_WRITE_TEST, "Read-write test failed!");

        Ok(())
    }
}
