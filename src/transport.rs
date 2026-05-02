use std::io::{Read, Write};

use serde::Serialize;

use crate::protocol::{Frame, MessageType, ProtocolError, read_frame};

#[derive(Debug)]
pub struct FramedReader<R> {
    inner: R,
}

impl<R> FramedReader<R> {
    pub fn new(inner: R) -> Self {
        Self { inner }
    }

    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R: Read> FramedReader<R> {
    pub fn read_frame(&mut self) -> Result<Option<Frame>, ProtocolError> {
        read_frame(&mut self.inner)
    }
}

#[derive(Debug)]
pub struct FramedWriter<W> {
    inner: W,
}

impl<W> FramedWriter<W> {
    pub fn new(inner: W) -> Self {
        Self { inner }
    }

    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write> FramedWriter<W> {
    pub fn write_frame(&mut self, frame: &Frame) -> Result<(), ProtocolError> {
        frame.write_to(&mut self.inner)
    }

    pub fn write_body<T>(
        &mut self,
        msg_type: MessageType,
        flags: u8,
        body: &T,
    ) -> Result<(), ProtocolError>
    where
        T: Serialize,
    {
        self.write_frame(&Frame::from_body(msg_type, flags, body)?)
    }

    pub fn flush(&mut self) -> Result<(), ProtocolError> {
        self.inner.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Capabilities, MessageType};
    use std::io::Cursor;

    #[test]
    fn framed_writer_and_reader_round_trip_body() {
        let mut writer = FramedWriter::new(Vec::new());
        let capabilities = Capabilities::default_supported();
        writer
            .write_body(MessageType::CAPABILITIES, 0, &capabilities)
            .unwrap();
        let bytes = writer.into_inner();

        let mut reader = FramedReader::new(Cursor::new(bytes));
        let frame = reader.read_frame().unwrap().unwrap();
        let decoded: Capabilities = frame.decode_body().unwrap();
        assert_eq!(decoded, capabilities);
    }
}
