use std::collections::HashMap;
use std::iter::FromIterator;
use std::io;
use std::io::prelude::*;
use std::net::TcpListener;
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::thread;
use std::convert::{TryInto, TryFrom};
use std::mem;

use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};

// NOTE: Modbus Messaging on TCP/IP Implementation Guide V1.0b
// NOTE: Modbus Application Protocol Specification V1.1a
// REF: http://www.Modbus-IDA.org
// Modbus TCP request frame:
// Transaction ID: 2 bytes  |  recopied
// Protocol ID:    2 bytes  |  recopied
// Length:         2 bytes  |  initialized by srv resp (nr of following bytes)
// Slave/Unit ID   1 byte   |  recopied (defualt 0xFF)
// ------------
// Function Code:  1 byte
// ------------
// Data:           n bytes
static HEADER_LENGTH: u8 = 7;
static MIN_DATA_LENGTH: u8 = 4;
static MAX_DATA_LENGTH: u16 = 260;

type Handlers = HashMap<u8, fn(&mut Register, &mut DefaultResponseWriter, Request)>;

#[derive(Clone)]
pub struct Server {
    address: String,
    shared: Arc<HashMap<u8, SharedData>>,
}

impl Server {
    pub fn new(addr: String) -> Server {
        Server {
            address: addr,
            shared: Arc::new(HashMap::new()),
        }
    }

    pub fn add_slave(&mut self, slave_id: u8, shared_data: SharedData) -> Result<(), &'static str> {
        let mut shared = Arc::new(HashMap::new());
        mem::swap(&mut shared, &mut self.shared);
        if let Ok(mut slaves) = Arc::try_unwrap(shared) {
            slaves.insert(slave_id, shared_data);
            self.shared = Arc::new(slaves);
        } else {
            return Err("failed")
        }
        Ok(())
    }

    pub fn run(&self) {
        let listener = TcpListener::bind(&self.address).unwrap();
        println!("listening started, ready to accept");
        for stream in listener.incoming() {
            let shared = Arc::clone(&self.shared);
            thread::spawn(move || {
                let mut stream = stream.unwrap();
                if let Err(code) = handle_conn(&shared, &mut stream) {
                    // TODO: use modbus exception codes
                    stream.write_fmt(format_args!("something went woring: exception {}\r\n", code)).unwrap();
                }
            });
        }
    }

}

pub struct SharedData {
    register: Mutex<Register>,
    handlers: Arc<Handlers>,
}

impl Default for SharedData {
    fn default() -> Self {
        SharedData{
            register: Mutex::new(Register{
                intern: vec![],
            }),
            handlers: {
                let mut h = Handlers::new();
                h.insert(0x03, Register::read_holding_registers);
                Arc::new(h)
            }
       }
    }
}

struct Register {
    intern: Vec<u8>,
}

impl Register {
    fn read_holding_registers(&mut self, w: &mut DefaultResponseWriter, r: Request) {
        // TODO read range from request
        let from = 0;
        let to = 5;
        w.payload = Vec::from_iter(self.intern[from..to].iter().cloned());
    }
}

fn handle_conn(slaves: &HashMap<u8, SharedData>, stream: &mut TcpStream) -> Result<(), u8> {
    // check min max length => prase_request() ?? TODO
    let r = Request::from_reader(stream).map_err(|_e|99u8)?;

    println!("request {:?}", r);
    let mut w = DefaultResponseWriter {
        header: r.header.clone(),
        fn_code: r.fn_code,
        payload: vec![],
    };

    // TODO: if not exist => error
    let shared = slaves.get(&r.header.slave_id()).ok_or(92u8)?;
    let h =shared.handlers.get(&r.fn_code()).ok_or(91u8)?;
    let mut reg = shared.register.lock().unwrap();
    h(&mut *reg, &mut w, r);
    w.compile(stream).map_err(|_e|98u8)?;
    Ok(())
}

// Header Modbus/TCP
#[derive(Debug, Clone, PartialEq)]
struct Header {
    // Transaction Identifier
    // It is used for transaction pairing, the Modbus server extract this from
    // the request and copies it in the response.
    transaction_id: u16,
    // Protocol Identifier
    // It is used for intra-system multiplexing. The Modbus protocol is
    // identified by the value 0x0000.
    protocol_id: u16,
    // Length
    // The length field is a byte count of the following fields, including
    // the Unit/Slave Identifier and datafields.
    length: u16,
    // Slave/Unit Identifier
    // This field is used for intra-system routing purpose.
    // Its typically used for serial connections
    // TCP default 0xFF
    slave_id: u8,
}

impl Header {
    pub fn from_reader<R: Read>(reader: &mut R) -> io::Result<Self> {
        Ok(Self {
            transaction_id: reader.read_u16::<BigEndian>()?,
            protocol_id: reader.read_u16::<BigEndian>()?,
            length: reader.read_u16::<BigEndian>()?,
            slave_id: reader.read_u8()?,
        })
    }

    // TransactionID returns the Modbus TransactionID which should normally be 1
    pub fn transaction_id(&self) -> u16 {
        self.transaction_id
    }

    // protocol_id returns the Modbus ProtocolID that must be 0x0000 on a
    // modbus/tcp connection
    pub fn protocol_id(&self) -> u16 {
        self.protocol_id
    }

    // slave_id returns a UnitID of a remote slave connected on a serialline or
    // on other buses
    pub fn slave_id(&self) -> u8 {
        self.slave_id
    }
}
// Request contains the modbus tcp header as well as the function code and the
// rest of the payload.
// The payload is stored internally as io.Reader
#[derive(Debug, PartialEq)]
struct Request {
    header: Header,
    fn_code: u8,
    payload: Vec<u8>,
}

impl Request {
    // from_reader writes the contents of p into the request. It returns the
    // number of bytes written. If len(p) < 1, it also returns an error
    // explaining why the write is short.
    pub fn from_reader<R: Read>(reader: &mut R) -> io::Result<Self> {
        Ok(Self {
            header: Header::from_reader(reader)?,
            fn_code: reader.read_u8()?,
            payload: {
                let mut buffer = vec![];
                reader.read_to_end(&mut buffer)?;
                buffer
            },
        })
    }

    // fn_code returns the modbus function code. This is used to deliver an
    // appropriate handler for the request.
    pub fn fn_code(&self) -> u8 {
        self.fn_code
    }
}

struct DefaultResponseWriter {
    header: Header,
    fn_code: u8,
    payload: Vec<u8>,
}

impl DefaultResponseWriter {
    pub fn compile<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        writer.write_u16::<BigEndian>(self.header.transaction_id())?;
        writer.write_u16::<BigEndian>(self.header.protocol_id())?;
        writer.write_u16::<BigEndian>((2 + self.payload.len()) as u16)?;
        writer.write_u8(self.fn_code)?;
        writer.write_all(&self.payload)?;
        Ok(())
    }

    pub fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let len = buf.len();
        if self.payload.len() + len > MAX_DATA_LENGTH.try_into().unwrap() {
            return Ok(0); // TODO: ErrModbusFrameLength
        }
        self.payload.extend_from_slice(buf);
        Ok(len)
    }
}

#[cfg(test)]
mod tests {
    // Note this useful idiom: importing names from outer (for mod tests) scope.
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_header() {
        let h = Header {
            transaction_id: 0x0102,
            protocol_id: 0x0304,
            length: 0x0506,
            slave_id: 0x07,
        };
        let bytes = vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07];
        assert_eq!(h, Header::from_reader(&mut Cursor::new(bytes)).unwrap());
    }

    #[test]
    fn test_request() {
        let r = Request {
            header: Header {
                transaction_id: 0x0102,
                protocol_id: 0x0304,
                length: 0x0506,
                slave_id: 0x07,
            },
            fn_code: 0x08,
            payload: vec![0x09, 0x10],
        };
        let bytes = vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x10];
        assert_eq!(r, Request::from_reader(&mut Cursor::new(bytes)).unwrap());
    }
}
