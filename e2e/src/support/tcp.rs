// Copyright lowRISC contributors.
// Licensed under the Apache License, Version 2.0, see LICENSE for details.
// SPDX-License-Identifier: Apache-2.0

//! A TCP-based Manticore `HostPort`.
//!
//! This module defines an ad-hoc binding of Cerberus over TCP (termed
//! "Cerberus over TCP"). This binding of Manticore implements the abstract
//! Cerberus header as a four-bytes, described as a packed C struct:
//! ```text
//! struct TcpCerberus {
//!   command_type: u8,
//!   payload_len: u16,
//! }
//! ```
//!
//! In a transport-agnostic-Cerberus world, the payload length bytes will
//! hopefully be removed.

use std::any::type_name;
use std::io::Read as _;
use std::io::Write as _;
use std::net::TcpListener;
use std::net::TcpStream;

use manticore::io;
use manticore::mem::Arena;
use manticore::net;
use manticore::net::host::HostPort;
use manticore::net::host::HostRequest;
use manticore::net::host::HostResponse;
use manticore::protocol;
use manticore::protocol::wire::FromWire;
use manticore::protocol::wire::ToWire;
use manticore::protocol::wire::WireEnum;
use manticore::protocol::Command;
use manticore::protocol::CommandType;
use manticore::protocol::Message;
use manticore::server;

/// Sends `req` to a virtual RoT listening on `localhost:{port}`, using
/// Cerberus-over-TCP.
///
/// Blocks until a response comes back.
pub fn send_local<'a, Cmd: Command<'a, CommandType = CommandType>>(
    port: u16,
    req: Cmd::Req,
    arena: &'a dyn Arena,
) -> Result<
    Result<Cmd::Resp, protocol::Error<'a, Cmd>>,
    server::Error<net::CerberusHeader>,
> {
    log::info!("connecting to 127.0.0.1:{}", port);
    let mut conn = TcpStream::connect(("127.0.0.1", port)).map_err(|e| {
        log::error!("{}", e);
        net::Error::Io(io::Error::Internal)
    })?;
    let mut writer = Writer::new(net::CerberusHeader {
        command: <Cmd::Req as Message>::TYPE,
    });
    log::info!("serializing {}", type_name::<Cmd::Req>());
    req.to_wire(&mut writer)?;
    writer.finish(&mut conn)?;

    /// Helper struct for exposing a TCP stream as a Manticore reader.
    struct Reader(TcpStream, usize);
    impl io::Read for Reader {
        fn read_bytes(&mut self, out: &mut [u8]) -> Result<(), io::Error> {
            let Reader(stream, len) = self;
            if *len < out.len() {
                return Err(io::Error::BufferExhausted);
            }
            stream.read_exact(out).map_err(|e| {
                log::error!("{}", e);
                io::Error::Internal
            })?;
            *len -= out.len();
            Ok(())
        }

        fn remaining_data(&self) -> usize {
            self.1
        }
    }
    #[allow(unsafe_code)]
    unsafe impl io::ReadZero<'_> for Reader {}

    log::info!("waiting for response");
    let (header, len) = header_from_wire(&mut conn)?;
    let mut r = Reader(conn, len);

    if header.command == <Cmd::Resp as Message>::TYPE {
        log::info!("deserializing {}", type_name::<Cmd::Resp>());
        Ok(Ok(FromWire::from_wire(&mut r, arena)?))
    } else if header.command == CommandType::Error {
        log::info!("deserializing {}", type_name::<protocol::Error<'a, Cmd>>());
        Ok(Err(FromWire::from_wire(&mut r, arena)?))
    } else {
        Err(net::Error::BadHeader.into())
    }
}

/// Parses a Cerberus-over-TCP header.
///
/// Returns a pair of abstract header and payload length.
fn header_from_wire(
    mut r: impl std::io::Read,
) -> Result<(net::CerberusHeader, usize), net::Error> {
    let mut header_bytes = [0u8; 3];
    r.read_exact(&mut header_bytes).map_err(|e| {
        log::error!("{}", e);
        net::Error::Io(io::Error::Internal)
    })?;
    let [cmd_byte, len_lo, len_hi] = header_bytes;

    let header = net::CerberusHeader {
        command: CommandType::from_wire_value(cmd_byte).ok_or_else(|| {
            log::error!("bad command byte: {}", cmd_byte);
            net::Error::BadHeader
        })?,
    };
    let len = u16::from_le_bytes([len_lo, len_hi]);
    Ok((header, len as usize))
}

/// A helper for constructing Cerberus-over-TCP messages.
///
/// Because the Cerberus-over-TCP header currently requires a length prefix for
/// the payload, we need to buffer the entire reply before writing the header.
///
/// This will be eliminated once length prefixes are no longer required
/// by the challenge protocol.
///
/// This type implements [`manticore::io::Write`].
struct Writer {
    header: net::CerberusHeader,
    buf: Vec<u8>,
}

impl Writer {
    /// Creates a new `Writer` that will encode the given abstract [`net::CerberusHeader`].
    pub fn new(header: net::CerberusHeader) -> Self {
        Self {
            header,
            buf: Vec::new(),
        }
    }

    /// Flushes the buffered data to the given [`std::io::Write`] (usually, a
    /// [`TcpStream`]).
    pub fn finish(self, mut w: impl std::io::Write) -> Result<(), net::Error> {
        let [len_lo, len_hi] = (self.buf.len() as u16).to_le_bytes();
        w.write_all(&[self.header.command.to_wire_value(), len_lo, len_hi])
            .map_err(|e| {
                log::error!("{}", e);
                net::Error::Io(io::Error::BufferExhausted)
            })?;
        w.write_all(&self.buf).map_err(|e| {
            log::error!("{}", e);
            net::Error::Io(io::Error::BufferExhausted)
        })?;
        Ok(())
    }
}

impl io::Write for Writer {
    fn write_bytes(&mut self, buf: &[u8]) -> Result<(), io::Error> {
        self.buf.extend_from_slice(buf);
        Ok(())
    }
}

/// A Cerberus-over-TCP implementation of [`HostPort`].
///
/// This type can be used to drive a Manticore server using a TCP port bound to
/// `localhost`. It also serves as an example for how an integration should
/// implement [`HostPort`] for their own transport.
pub struct TcpHostPort(Inner);

/// The "inner" state of the `HostPort`. This type is intended to carry the state
/// and functionality for an in-process request/response flow, without making it
/// accessible to outside callers except through the associated [`manticore::net`]
/// trait objects.
///
/// Most implementations of `HostPort` will follow this "nesting doll" pattern.
///
/// This type implements [`HostRequest`], [`HostReply`], and [`manticore::io::Read`],
/// though users may only move from one trait implementation to the other by calling
/// methods like `reply()` and `payload()`.
struct Inner {
    listener: TcpListener,
    // State for `HostRequest`: a parsed header, the length of the payload, and
    // a stream to read it from.
    stream: Option<(net::CerberusHeader, usize, TcpStream)>,
    // State for `HostResponse`: a `Writer` to dump the response bytes into.
    output_buffer: Option<Writer>,
}

impl TcpHostPort {
    /// Binds a new `TcpHostPort` to an open port.
    pub fn bind() -> Result<Self, net::Error> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| {
            log::error!("{}", e);
            net::Error::Io(io::Error::Internal)
        })?;
        Ok(Self(Inner {
            listener,
            stream: None,
            output_buffer: None,
        }))
    }

    /// Returns the TCP port this `HostPort` is bound to.
    pub fn port(&self) -> u16 {
        self.0.listener.local_addr().unwrap().port()
    }
}

impl<'req> HostPort<'req, net::CerberusHeader> for TcpHostPort {
    fn receive(
        &mut self,
    ) -> Result<&mut dyn HostRequest<'req, net::CerberusHeader>, net::Error>
    {
        let inner = &mut self.0;
        inner.stream = None;

        log::info!("blocking on listener");
        let (mut stream, _) = inner.listener.accept().map_err(|e| {
            log::error!("{}", e);
            net::Error::Io(io::Error::Internal)
        })?;

        log::info!("parsing header");
        let (header, len) = header_from_wire(&mut stream)?;
        inner.stream = Some((header, len, stream));

        Ok(inner)
    }
}

impl<'req> HostRequest<'req, net::CerberusHeader> for Inner {
    fn header(&self) -> Result<net::CerberusHeader, net::Error> {
        if self.output_buffer.is_some() {
            log::error!("header() called out-of-order");
            return Err(net::Error::OutOfOrder);
        }
        self.stream
            .as_ref()
            .map(|(h, _, _)| *h)
            .ok_or(net::Error::Disconnected)
    }

    fn payload(&mut self) -> Result<&mut dyn io::ReadZero<'req>, net::Error> {
        if self.stream.is_none() {
            log::error!("payload() called out-of-order");
            return Err(net::Error::Disconnected);
        }
        if self.output_buffer.is_some() {
            log::error!("payload() called out-of-order");
            return Err(net::Error::OutOfOrder);
        }

        Ok(self)
    }

    fn reply(
        &mut self,
        header: net::CerberusHeader,
    ) -> Result<&mut dyn HostResponse<'req>, net::Error> {
        if self.stream.is_none() {
            log::error!("payload() called out-of-order");
            return Err(net::Error::Disconnected);
        }
        if self.output_buffer.is_some() {
            log::error!("payload() called out-of-order");
            return Err(net::Error::OutOfOrder);
        }

        self.output_buffer = Some(Writer::new(header));
        Ok(self)
    }
}

impl HostResponse<'_> for Inner {
    fn sink(&mut self) -> Result<&mut dyn io::Write, net::Error> {
        if self.stream.is_none() {
            log::error!("sink() called out-of-order");
            return Err(net::Error::Disconnected);
        }

        self.output_buffer
            .as_mut()
            .map(|w| w as &mut dyn io::Write)
            .ok_or(net::Error::OutOfOrder)
    }

    fn finish(&mut self) -> Result<(), net::Error> {
        match self {
            Inner {
                stream: Some((_, _, stream)),
                output_buffer: Some(_),
                ..
            } => {
                log::info!("sending reply");
                self.output_buffer.take().unwrap().finish(&mut *stream)?;
                stream.flush().map_err(|e| {
                    log::error!("{}", e);
                    net::Error::Io(io::Error::Internal)
                })?;
                self.stream = None;
                self.output_buffer = None;
                Ok(())
            }
            _ => Err(net::Error::Disconnected),
        }
    }
}

impl io::Read for Inner {
    fn read_bytes(&mut self, out: &mut [u8]) -> Result<(), io::Error> {
        let (_, len, stream) =
            self.stream.as_mut().ok_or(io::Error::Internal)?;
        if *len < out.len() {
            return Err(io::Error::BufferExhausted);
        }
        stream.read_exact(out).map_err(|e| {
            log::error!("{}", e);
            io::Error::Internal
        })?;
        *len -= out.len();
        Ok(())
    }

    fn remaining_data(&self) -> usize {
        self.stream.as_ref().map(|(_, len, _)| *len).unwrap_or(0)
    }
}
#[allow(unsafe_code)]
unsafe impl io::ReadZero<'_> for Inner {}