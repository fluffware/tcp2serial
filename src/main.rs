use clap::{CommandFactory, FromArgMatches, Parser};
use futures::stream::FuturesUnordered;
use futures_util::StreamExt;
use log::{debug, error};
use std::future::Future;
use std::marker::Unpin;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::process::ExitCode;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;
use tcp2serial::daemon;
use tcp2serial::shared_resource::Request;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::signal;
use tokio::sync::Notify;
use tokio_serial::{Parity, SerialStream};

type DynResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

struct FutureOption<F>(Option<F>);

impl<F> Future for FutureOption<F>
where
    F: Future,
{
    type Output = F::Output;
    fn poll(self: Pin<&mut Self>, ctxt: &mut Context<'_>) -> Poll<Self::Output> {
        match unsafe { Pin::new_unchecked(&mut self.get_unchecked_mut().0) }.as_pin_mut() {
            Some(f) => f.poll(ctxt),
            None => Poll::Pending,
        }
    }
}

impl<F> FutureOption<F>
where
    F: Future,
{
    fn replace(self: &mut Pin<&mut Self>, r: Option<F>) {
        let opt: &mut Self = unsafe { self.as_mut().get_unchecked_mut() };
        opt.0 = r;
    }
}

async fn connection_handler<S>(
    mut stream: TcpStream,
    ser_shared: Request<S>,
    cancel: Arc<Notify>,
    switch_delay: Duration,
    framing_delay: Duration,
) -> DynResult<()>
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let mut ser_buf = [0u8; 256];
    let mut net_buf = [0u8; 256];

    let mut first = true;
    'main_loop: loop {
        let mut ser;
        if first {
            first = false;
            ser = ser_shared.request().await;
        } else {
            // Wait for some data from the net before claiming the serial device
            'start_loop: loop {
                tokio::select! {
                    res = stream.read(&mut net_buf) => {
                        let rlen = res?;
                        if rlen == 0 {
                            break 'main_loop;
                        }
                        ser = ser_shared.request().await;

                        ser.write_all(&net_buf[0..rlen]).await?;
                        break 'start_loop;
                    }
                    _ = cancel.notified() => {
                        break 'main_loop;
                    }
                }
            }
        }
        let mut timed_out = false;
        let mut ser_buf_end = 0;
        let framing_timeout = FutureOption::<tokio::time::Sleep>(None);
        tokio::pin!(framing_timeout);
        'read_loop: loop {
            tokio::select! {
                res = ser.read(&mut ser_buf[ser_buf_end..]) => {
                    match res {
                        Ok(rlen) => {
                            ser_buf_end += rlen;
                            if framing_delay == Duration::ZERO || ser_buf_end == ser_buf.len() {
                                stream.write_all(&ser_buf[0..ser_buf_end]).await?;
                                ser_buf_end = 0;
                            } else {
                                framing_timeout.replace(Some(tokio::time::sleep(framing_delay)));
                            }
                        }
                        Err(e) => {
                            return Err(e.into());
                        }
                    }
                }
                _res = framing_timeout.as_mut() => {
                    framing_timeout.replace(None);
                    stream.write_all(&ser_buf[0..ser_buf_end]).await?;
                    ser_buf_end = 0;
                }
                res = stream.read(&mut net_buf) => {
                    match res {
                        Ok(rlen) => {
                            if rlen == 0 {
                                break 'main_loop;
                            }
                            ser.write_all(&net_buf[0..rlen]).await?;
                            timed_out = false;
                        }
                        Err(e) => {
                            return Err(e.into());
                        }
                    }
                }
                _ = cancel.notified() => {
                    break 'main_loop;
                }
                _ = tokio::time::sleep(switch_delay) => {
                    timed_out = true;
                }
                _ = ser_shared.requested(), if timed_out => {
                    break 'read_loop;
                }
            }
        }
    }
    Ok(())
}

async fn tcp_listener<S>(
    socket: Vec<SocketAddr>,
    ser: Request<S>,
    cancel: Arc<Notify>,
    switch_delay: Duration,
    framing_delay: Duration,
) -> DynResult<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut child_handlers = FuturesUnordered::new();
    // Dummy task that blocks until canceled
    let cancel_block = cancel.clone();
    child_handlers.push(tokio::spawn(async move {
        cancel_block.notified().await;
        Ok(())
    }));
    let listener = TcpListener::bind(socket.as_slice()).await?;
    loop {
        tokio::select! {
            res = listener.accept() => {
                match res {
                    Ok((stream, sock)) => {
                        debug!("Connection from {sock}");
                        let h = tokio::spawn(connection_handler(stream, ser.clone(),
                                                                cancel.clone(),
                                                                switch_delay,framing_delay));
                        child_handlers.push(h);
                    }
                    Err(e) => {
                        error!("accept failed: {e}");
                    }
                }
            }
            next_handler = child_handlers.next() => {
                match next_handler {
                    Some(res) => {
                        match res {
                            Ok(Err(e)) => {
				return Err(format!("Connection handler exited with error: {e}").into());
                            }
                            Ok(Ok(())) => {}
                            Err(e) => {
				return Err(format!("Connection handler failed: {e}").into());
                            }
                        }
                    }
                    None => break
                }
            }
        }
    }
    Ok(())
}

const DEFAULT_SERIAL_DEVICE: &str = "/dev/ttyUSB0";
const DEFAULT_TCP_PORT: u16 = 10001;

const DEFAULT_SERIAL_SPEED: u32 = 9600;

const DEFAULT_SWITCH_DELAY: u64 = 1000; // 1s
const DEFAULT_FRAMING_DELAY: u64 = 0; // No delay

#[derive(Parser, Debug)]
struct CmdArgs {
    /// Serial port
    #[arg(long, short='d', default_value_t=DEFAULT_SERIAL_DEVICE.to_string())]
    serial_device: String,
    /// TCP port
    #[arg(long, short='p', default_value_t=DEFAULT_TCP_PORT)]
    tcp_port: u16,
    /// Local IP address
    #[arg(long, short = 'b')]
    bind: Option<IpAddr>,
    /// Serial speed (bps)
    #[arg(long, short = 's', default_value_t=DEFAULT_SERIAL_SPEED)]
    serial_speed: u32,
    /// Select odd parity
    #[arg(long, short = 'o')]
    odd_parity: bool,
    /// Select even parity
    #[arg(long, short = 'e', conflicts_with("odd_parity"))]
    even_parity: bool,
    /// Minimum time (in milliseconds) from idle communication
    /// until switch to different TCP connection
    #[arg(long, short = 't', default_value_t=DEFAULT_SWITCH_DELAY)]
    switch_delay: u64,
    /// Maximum time (in milliseconds) between reception of serial data
    /// that will be sent as a single packet.
    #[arg(long, default_value_t=DEFAULT_FRAMING_DELAY)]
    framing_delay: u64,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cmd = CmdArgs::command();
    let cmd = daemon::add_args(cmd);
    let matches = cmd.get_matches();
    let args = match CmdArgs::from_arg_matches(&matches) {
        Ok(a) => a,
        Err(e) => {
            error!("{e}");
            return ExitCode::FAILURE;
        }
    };
    daemon::start(&matches);
    let parity = if args.even_parity {
        Parity::Even
    } else if args.odd_parity {
        Parity::Odd
    } else {
        Parity::None
    };
    let ser_conf = tokio_serial::new(args.serial_device, args.serial_speed).parity(parity);
    let ser = Request::new(match SerialStream::open(&ser_conf) {
        Ok(s) => s,
        Err(e) => {
            error!("Failed to open serial device: {e}");
            return ExitCode::FAILURE;
        }
    });

    let bind_addr: Vec<SocketAddr> = if let Some(addr) = args.bind {
        vec![SocketAddr::from((addr, args.tcp_port))]
    } else {
        vec![
            SocketAddr::from((Ipv6Addr::UNSPECIFIED, args.tcp_port)),
            SocketAddr::from((Ipv4Addr::UNSPECIFIED, args.tcp_port)),
        ]
    };
    let cancel = Arc::new(Notify::new());
    let net_task = tokio::spawn(tcp_listener(
        bind_addr,
        ser,
        cancel.clone(),
        Duration::from_millis(args.switch_delay),
        Duration::from_millis(args.framing_delay),
    ));
    tokio::pin!(net_task);
    daemon::ready();
    'main_loop: loop {
        tokio::select! {
            res = signal::ctrl_c() => {
                if let Err(e) = res {
                    error!("Failed to wait for ctrl-c: {}",e);
                }
                cancel.notify_waiters();

            },
            res = net_task.as_mut() => {
                match res {
                    Ok(res) => {
                        if let Err(e) = res {
                            error!("{e}")
                        }
                    },
                    Err(e) => error!("Network task failed: {e}"),
                }
                break 'main_loop;
            }
        }
    }

    daemon::exiting();
    ExitCode::SUCCESS
}
