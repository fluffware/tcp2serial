pub mod shared_resource;

#[cfg(feature = "systemd")]
mod systemd;

#[cfg(feature = "syslog")]
mod syslog;


#[cfg(all(not(feature = "systemd"),not(feature = "syslog")))]
mod no_systemd;

pub mod daemon {
    #[cfg(all(not(feature = "systemd"),not(feature = "syslog")))]
    pub use crate::no_systemd::{add_args, exiting, ready, start};
    #[cfg(feature = "systemd")]
    pub use crate::systemd::{add_args, exiting, ready, start};
    #[cfg(feature = "syslog")]
    pub use crate::syslog::{add_args, exiting, ready, start};
}
