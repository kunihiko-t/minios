use super::SectorReader;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdError {
    Transport,
    Response(u8),
    Timeout,
    Voltage,
    Unsupported,
    Token(u8),
    Crc,
}

pub trait Bus {
    fn select(&mut self, active: bool) -> Result<(), SdError>;
    fn transfer(&mut self, tx: u8) -> Result<u8, SdError>;
    fn delay_ms(&mut self, milliseconds: u32);
}

pub struct SdCard<B> {
    bus: B,
}

fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn cleanup<B: Bus>(bus: &mut B) -> Result<(), SdError> {
    let select = bus.select(false);
    let transfer = bus.transfer(0xff).map(|_| ());
    match (select, transfer) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), _) => Err(error),
        (_, Err(error)) => Err(error),
    }
}

fn cmd<B: Bus>(bus: &mut B, command: u8, argument: u32, crc: u8) -> Result<u8, SdError> {
    bus.select(false)?;
    bus.transfer(0xff)?;
    bus.select(true)?;
    bus.transfer(0xff)?;

    let frame = [
        0x40 | command,
        (argument >> 24) as u8,
        (argument >> 16) as u8,
        (argument >> 8) as u8,
        argument as u8,
        crc,
    ];
    for &byte in &frame {
        bus.transfer(byte)?;
    }
    for _ in 0..16 {
        let response = bus.transfer(0xff)?;
        if response & 0x80 == 0 {
            return Ok(response);
        }
    }
    Err(SdError::Timeout)
}

fn read_bytes<B: Bus>(bus: &mut B, destination: &mut [u8]) -> Result<(), SdError> {
    for byte in destination {
        *byte = bus.transfer(0xff)?;
    }
    Ok(())
}

fn inner_init<B: Bus>(bus: &mut B) -> Result<(), SdError> {
    bus.delay_ms(1);
    bus.select(false)?;
    for _ in 0..10 {
        bus.transfer(0xff)?;
    }

    let mut idle = false;
    for _ in 0..10 {
        match cmd(bus, 0, 0, 0x95) {
            Ok(1) => {
                idle = true;
                break;
            }
            Ok(response) => return Err(SdError::Response(response)),
            Err(SdError::Timeout) => bus.delay_ms(1),
            Err(error) => return Err(error),
        }
    }
    if !idle {
        return Err(SdError::Timeout);
    }

    let response = cmd(bus, 8, 0x1aa, 0x87)?;
    if response != 1 {
        return Err(SdError::Response(response));
    }
    let mut echo = [0u8; 4];
    read_bytes(bus, &mut echo)?;
    if echo != [0, 0, 1, 0xaa] {
        return Err(SdError::Voltage);
    }

    let mut ready = false;
    for _ in 0..2_000 {
        let response = cmd(bus, 55, 0, 0x01)?;
        if response != 0 && response != 1 {
            return Err(SdError::Response(response));
        }
        match cmd(bus, 41, 0x4000_0000, 0x01)? {
            0 => {
                ready = true;
                break;
            }
            1 => bus.delay_ms(1),
            response => return Err(SdError::Response(response)),
        }
    }
    if !ready {
        return Err(SdError::Timeout);
    }

    let response = cmd(bus, 58, 0, 0x01)?;
    if response != 0 {
        return Err(SdError::Response(response));
    }
    let mut ocr = [0u8; 4];
    read_bytes(bus, &mut ocr)?;
    let ocr = u32::from_be_bytes(ocr);
    if ocr & (1 << 31) == 0 {
        return Err(SdError::Voltage);
    }
    if ocr & (1 << 30) == 0 {
        return Err(SdError::Unsupported);
    }
    if ocr & ((1 << 20) | (1 << 21)) == 0 {
        return Err(SdError::Voltage);
    }
    Ok(())
}

fn inner_read<B: Bus>(bus: &mut B, lba: u32, destination: &mut [u8; 512]) -> Result<(), SdError> {
    let response = cmd(bus, 17, lba, 0x01)?;
    if response != 0 {
        return Err(SdError::Response(response));
    }

    let mut data_token = false;
    for _ in 0..12_500 {
        let token = bus.transfer(0xff)?;
        if token == 0xff {
            continue;
        }
        if token == 0xfe {
            data_token = true;
            break;
        }
        return Err(SdError::Token(token));
    }
    if !data_token {
        return Err(SdError::Timeout);
    }

    read_bytes(bus, destination)?;
    let high = bus.transfer(0xff)?;
    let low = bus.transfer(0xff)?;
    let card_crc = ((high as u16) << 8) | low as u16;
    if crc16(destination) != card_crc {
        return Err(SdError::Crc);
    }
    Ok(())
}

impl<B: Bus> SdCard<B> {
    pub fn init(mut bus: B) -> Result<Self, SdError> {
        match inner_init(&mut bus) {
            Ok(()) => cleanup(&mut bus).map(|()| Self { bus }),
            Err(error) => {
                let _ = cleanup(&mut bus);
                Err(error)
            }
        }
    }
}

impl<B: Bus> SectorReader for SdCard<B> {
    type Error = SdError;

    fn read_sector(&mut self, lba: u32, destination: &mut [u8; 512]) -> Result<(), Self::Error> {
        match inner_read(&mut self.bus, lba, destination) {
            Ok(()) => cleanup(&mut self.bus),
            Err(error) => {
                let _ = cleanup(&mut self.bus);
                Err(error)
            }
        }
    }
}

#[cfg(test)]
impl<B> SdCard<B> {
    pub fn into_bus(self) -> B {
        self.bus
    }

    pub fn bus(&self) -> &B {
        &self.bus
    }

    pub fn assume_initialized_for_test(bus: B) -> Self {
        Self { bus }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{Bus, SdCard, SdError};
    use crate::storage::SectorReader;
    use std::vec::Vec;

    #[derive(Clone, Copy)]
    enum Scenario {
        SuccessfulInit,
        InitVoltageMismatch,
        InitUnsupportedOcr,
        InitTimeout,
        ZeroSector,
        BadCrc,
        BadToken,
        TransportFailure,
    }

    pub struct FakeBus {
        scenario: Scenario,
        selected: bool,
        waiting_for_frame: bool,
        frame: [u8; 6],
        frame_len: usize,
        response: Vec<u8>,
        commands: Vec<u8>,
        last_command: (u8, u32),
        fail_once: bool,
    }

    impl FakeBus {
        fn new(scenario: Scenario) -> Self {
            Self {
                scenario,
                selected: false,
                waiting_for_frame: false,
                frame: [0; 6],
                frame_len: 0,
                response: Vec::new(),
                commands: Vec::new(),
                last_command: (0, 0),
                fail_once: false,
            }
        }

        fn successful_init() -> Self {
            Self::new(Scenario::SuccessfulInit)
        }

        fn initialized_with_zero_sector() -> Self {
            Self::new(Scenario::ZeroSector)
        }

        fn initialized_with_bad_crc() -> Self {
            Self::new(Scenario::BadCrc)
        }

        fn initialized_with_bad_token() -> Self {
            Self::new(Scenario::BadToken)
        }

        fn initialized_with_transport_failure() -> Self {
            let mut bus = Self::new(Scenario::TransportFailure);
            bus.fail_once = true;
            bus
        }

        fn voltage_mismatch() -> Self {
            Self::new(Scenario::InitVoltageMismatch)
        }

        fn unsupported_ocr() -> Self {
            Self::new(Scenario::InitUnsupportedOcr)
        }

        fn init_timeout() -> Self {
            Self::new(Scenario::InitTimeout)
        }

        fn commands(&self) -> Vec<u8> {
            self.commands.clone()
        }

        fn is_selected(&self) -> bool {
            self.selected
        }

        fn last_command(&self) -> (u8, u32) {
            self.last_command
        }

        fn command_response(&mut self, command: u8) {
            self.response.clear();
            match self.scenario {
                Scenario::SuccessfulInit => match command {
                    0 => self.response.push(1),
                    8 => self.response.extend_from_slice(&[1, 0, 0, 1, 0xaa]),
                    55 => self.response.push(1),
                    41 => self.response.push(0),
                    58 => self.response.extend_from_slice(&[0, 0xc0, 0x10, 0, 0]),
                    _ => panic!("unexpected command {command}"),
                },
                Scenario::InitVoltageMismatch => match command {
                    0 => self.response.push(1),
                    8 => self.response.extend_from_slice(&[1, 0, 0, 1, 0xab]),
                    _ => panic!("unexpected command {command}"),
                },
                Scenario::InitUnsupportedOcr => match command {
                    0 => self.response.push(1),
                    8 => self.response.extend_from_slice(&[1, 0, 0, 1, 0xaa]),
                    55 => self.response.push(1),
                    41 => self.response.push(0),
                    58 => self.response.extend_from_slice(&[0, 0x80, 0x10, 0, 0]),
                    _ => panic!("unexpected command {command}"),
                },
                Scenario::InitTimeout => {
                    if command != 0 {
                        panic!("unexpected command {command}");
                    }
                    self.response.push(0xff);
                }
                Scenario::ZeroSector | Scenario::BadCrc | Scenario::BadToken => {
                    if command != 17 {
                        panic!("unexpected command {command}");
                    }
                    self.response.push(0);
                    self.response.push(match self.scenario {
                        Scenario::ZeroSector | Scenario::BadCrc => 0xfe,
                        Scenario::BadToken => 0xfd,
                        _ => unreachable!(),
                    });
                    self.response.extend_from_slice(&[0; 512]);
                    if matches!(self.scenario, Scenario::BadCrc) {
                        self.response.extend_from_slice(&[0xff, 0xff]);
                    } else {
                        self.response.extend_from_slice(&[0, 0]);
                    }
                }
                Scenario::TransportFailure => {
                    if command != 17 {
                        panic!("unexpected command {command}");
                    }
                    self.response.push(0);
                    self.response.push(0xfe);
                }
            }
        }
    }

    impl Bus for FakeBus {
        fn select(&mut self, active: bool) -> Result<(), SdError> {
            self.selected = active;
            if active {
                self.waiting_for_frame = true;
                self.frame_len = 0;
            }
            Ok(())
        }

        fn transfer(&mut self, tx: u8) -> Result<u8, SdError> {
            if self.fail_once && self.selected && !self.waiting_for_frame && self.frame_len == 6 {
                self.fail_once = false;
                return Err(SdError::Transport);
            }
            if !self.selected {
                return Ok(0xff);
            }
            if self.waiting_for_frame {
                if tx != 0xff {
                    panic!("missing command preamble");
                }
                self.waiting_for_frame = false;
                return Ok(0xff);
            }
            if self.frame_len < self.frame.len() {
                self.frame[self.frame_len] = tx;
                self.frame_len += 1;
                if self.frame_len == self.frame.len() {
                    let command = self.frame[0] & 0x3f;
                    let argument = u32::from_be_bytes([
                        self.frame[1],
                        self.frame[2],
                        self.frame[3],
                        self.frame[4],
                    ]);
                    if !matches!(command, 0 | 8 | 17 | 41 | 55 | 58) {
                        panic!("unexpected command {command}");
                    }
                    self.commands.push(command);
                    self.last_command = (command, argument);
                    self.command_response(command);
                }
                return Ok(0xff);
            }
            if self.response.is_empty() {
                return Ok(0xff);
            }
            Ok(self.response.remove(0))
        }

        fn delay_ms(&mut self, _milliseconds: u32) {}
    }

    impl Drop for FakeBus {
        fn drop(&mut self) {
            assert!(!self.selected, "fake bus dropped while selected");
        }
    }

    #[test]
    fn crc16_matches_the_standard_vector() {
        assert_eq!(super::crc16(b"123456789"), 0x31c3);
    }

    #[test]
    fn initialization_sends_only_the_read_only_command_sequence() {
        let mut bus = FakeBus::successful_init();
        let card = SdCard::init(bus).unwrap();
        bus = card.into_bus();
        assert_eq!(bus.commands(), [0, 8, 55, 41, 58]);
        assert!(!bus.is_selected());
    }

    #[test]
    fn sector_read_uses_the_lba_as_the_cmd17_argument() {
        let bus = FakeBus::initialized_with_zero_sector();
        let mut card = SdCard::assume_initialized_for_test(bus);
        let mut sector = [0xff; 512];
        card.read_sector(7, &mut sector).unwrap();
        assert_eq!(card.bus().last_command(), (17, 7));
        assert_eq!(sector, [0; 512]);
    }

    #[test]
    fn bad_data_crc_is_rejected_and_chip_select_is_released() {
        let bus = FakeBus::initialized_with_bad_crc();
        let mut card = SdCard::assume_initialized_for_test(bus);
        let mut sector = [0; 512];
        assert_eq!(card.read_sector(0, &mut sector), Err(SdError::Crc));
        assert!(!card.bus().is_selected());
    }

    #[test]
    fn cmd8_voltage_mismatch_is_rejected_and_chip_select_is_released() {
        let bus = FakeBus::voltage_mismatch();
        assert_eq!(SdCard::init(bus).err(), Some(SdError::Voltage));
    }

    #[test]
    fn non_sdhc_ocr_is_rejected_and_chip_select_is_released() {
        let bus = FakeBus::unsupported_ocr();
        assert_eq!(SdCard::init(bus).err(), Some(SdError::Unsupported));
    }

    #[test]
    fn initialization_timeout_is_reported() {
        let bus = FakeBus::init_timeout();
        assert_eq!(SdCard::init(bus).err(), Some(SdError::Timeout));
    }

    #[test]
    fn bad_data_token_is_rejected_and_chip_select_is_released() {
        let bus = FakeBus::initialized_with_bad_token();
        let mut card = SdCard::assume_initialized_for_test(bus);
        let mut sector = [0; 512];
        assert_eq!(card.read_sector(0, &mut sector), Err(SdError::Token(0xfd)));
        assert!(!card.bus().is_selected());
    }

    #[test]
    fn transport_failure_releases_chip_select() {
        let bus = FakeBus::initialized_with_transport_failure();
        let mut card = SdCard::assume_initialized_for_test(bus);
        let mut sector = [0; 512];
        assert_eq!(card.read_sector(0, &mut sector), Err(SdError::Transport));
        assert!(!card.bus().is_selected());
    }
}
