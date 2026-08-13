//! Finding the other application on the church's wifi.
//!
//! Most churches that run both products run them on two machines in the same
//! room: Pulpitry at the operator's desk driving the projector, Castavox on the
//! streaming machine. Today each one listens to the preacher separately, which
//! means two microphones, two transcriptions, and — on a subscription — two
//! streams of audio billed for one sermon.
//!
//! It should be one. Pulpitry listens, and Castavox shows what Pulpitry staged.
//!
//! # What crosses the wifi, and what does not
//!
//! A verse reference and its text. Not pixels.
//!
//! | | Bandwidth |
//! | --- | --- |
//! | NDI, 1080p60 | ~100 Mbit/s |
//! | NDI HX | ~20 Mbit/s |
//! | Hosted transcription, upstream | 0.26 Mbit/s |
//! | What this sends | ~0.0005 Mbit/s |
//!
//! NDI would work and is the wrong transport here, for a reason specific to
//! churches: it is built for wired gigabit, church wifi is not that, and it is
//! the *same link* the hosted recogniser depends on. Sending a hundred megabits
//! a second to communicate "John 3:16" would have the mirror competing for
//! bandwidth with the thing that makes the product work.
//!
//! # Which end advertises
//!
//! Pulpitry advertises; Castavox goes looking. That follows from where the
//! pairing code is shown — Pulpitry displays a short code and Castavox is given
//! it — and it is the right way round for a second reason: the machine holding
//! the transcript is the one worth finding, and a church may well run two
//! Castavox instances against one desk.
//!
//! # This module finds; it does not connect
//!
//! Discovery answers "what is out there and at which address". Everything about
//! trust — the pairing code, the token, what a paired peer is allowed to
//! send — belongs to the connection and is deliberately not here. An
//! advertisement is public by nature: anything on the network can see it, so
//! nothing in it is a secret and nothing in it is taken on faith.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use anyhow::{Context, Result};
use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};

/// The service type both ends agree on.
///
/// `_castavox._tcp` rather than something per-product: the pair is the thing
/// being discovered, and a name that mentioned one product would be wrong the
/// first time we let two Castavoxes share a desk.
pub const SERVICE_TYPE: &str = "_castavox._tcp.local.";

/// TXT keys. Spelled out because a typo in one of these is a peer that is
/// found, looks empty, and is never explained.
const TXT_NAME: &str = "name";
const TXT_PRODUCT: &str = "product";
const TXT_VERSION: &str = "version";
const TXT_ID: &str = "id";

/// How long a browse waits before giving up on an empty network.
///
/// Long enough for a multicast round trip on wifi that is busy carrying a
/// service, short enough that a dialog does not look hung. Callers wanting to
/// keep looking should keep the receiver rather than raise this.
pub const BROWSE_TIMEOUT: Duration = Duration::from_secs(5);

/// A desk somebody could mirror from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    /// A stable identifier for this installation, from its TXT record.
    ///
    /// Not the hostname and not the address, both of which change: a laptop
    /// gets a new DHCP lease, and a church renames a machine. A pairing is
    /// remembered against this, so it survives both.
    pub id: String,
    /// What to call it on screen — the machine's name, as its owner set it.
    pub name: String,
    /// "pulpitry" or "castavox".
    pub product: String,
    pub version: String,
    pub addresses: Vec<IpAddr>,
    pub port: u16,
}

impl Peer {
    /// Where to connect, preferring IPv4.
    ///
    /// Not a preference on the merits. Both work, and IPv6 link-local addresses
    /// carry a zone index that has to be right for a connection to succeed —
    /// which is one more thing to get wrong on a network nobody administers, in
    /// exchange for nothing a church would notice.
    pub fn address(&self) -> Option<IpAddr> {
        self.addresses
            .iter()
            .find(|address| address.is_ipv4())
            .or_else(|| self.addresses.first())
            .copied()
    }
}

/// An advertisement, which lasts as long as this is held.
///
/// Dropping it withdraws the service rather than leaving it to time out. A desk
/// that has closed should stop being offered immediately: the alternative is an
/// operator picking a machine from a list that is no longer there, and blaming
/// the pairing for a failure that is really an expiry.
pub struct Advertisement {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Advertisement {
    /// Stops advertising. Called by `Drop`, and exposed for a caller that wants
    /// to know it finished.
    pub fn withdraw(&self) -> Result<()> {
        self.daemon
            .unregister(&self.fullname)
            .context("could not withdraw the mirror advertisement")?;
        Ok(())
    }
}

impl Drop for Advertisement {
    fn drop(&mut self) {
        let _ = self.withdraw();
        let _ = self.daemon.shutdown();
    }
}

/// Offers this machine to anything looking for a desk to mirror.
///
/// `id` should be stable across restarts — the same identifier a pairing is
/// remembered against. `name` is for a human choosing from a list.
pub fn advertise(id: &str, name: &str, product: &str, version: &str, port: u16) -> Result<Advertisement> {
    let daemon = ServiceDaemon::new().context("could not start mDNS")?;

    let mut txt: HashMap<String, String> = HashMap::new();
    txt.insert(TXT_ID.into(), id.into());
    txt.insert(TXT_NAME.into(), name.into());
    txt.insert(TXT_PRODUCT.into(), product.into());
    txt.insert(TXT_VERSION.into(), version.into());

    // The instance name is derived from the id rather than from the machine
    // name. Machine names collide -- a church that buys two identical laptops
    // has two called the same thing -- and mDNS resolves a collision by
    // renaming, which would silently change the thing a pairing was made
    // against.
    let instance = instance_name(id);

    let service = ServiceInfo::new(
        SERVICE_TYPE,
        &instance,
        &format!("{instance}.local."),
        // Empty: the daemon fills in this machine's addresses, and enumerating
        // them here would pin the advertisement to whichever interface was up
        // when it started. A laptop moving from wifi to ethernet mid-setup is
        // not an unusual thing in a church hall.
        "",
        port,
        txt,
    )
    .context("could not describe the mirror service")?
    .enable_addr_auto();

    let fullname = service.get_fullname().to_string();
    daemon.register(service).context("could not advertise the mirror")?;

    Ok(Advertisement { daemon, fullname })
}

/// Watches for desks appearing and disappearing.
///
/// The receiver stays live: a dialog left open sees a machine that is switched
/// on afterwards, which is what happens when somebody is setting both up at
/// once and reaches the second one second.
pub fn browse() -> Result<(ServiceDaemon, Receiver<Discovery>)> {
    let daemon = ServiceDaemon::new().context("could not start mDNS")?;
    let events = daemon.browse(SERVICE_TYPE).context("could not look for a desk")?;
    let (tx, rx) = channel();

    std::thread::Builder::new()
        .name("castavox-mirror-browse".into())
        .spawn(move || {
            while let Ok(event) = events.recv() {
                let message = match event {
                    ServiceEvent::ServiceResolved(info) => Discovery::Found(peer_of(&info)),
                    // Withdrawn, or expired. Either way it should leave a list
                    // before somebody picks it.
                    ServiceEvent::ServiceRemoved(_, fullname) => Discovery::Lost(fullname),
                    _ => continue,
                };
                // The other end has stopped listening: nothing to do but stop.
                if tx.send(message).is_err() {
                    break;
                }
            }
        })
        .context("could not start the discovery thread")?;

    Ok((daemon, rx))
}

/// What a browse reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Discovery {
    Found(Peer),
    /// The mDNS full name, which is what `Peer::fullname` would have been.
    Lost(String),
}

/// Everything on the network right now, gathered for the length of one timeout.
///
/// For a settings dialog that wants a list rather than a stream. A peer seen
/// twice — mDNS repeats itself — appears once, keyed on its id.
pub fn find(timeout: Duration) -> Result<Vec<Peer>> {
    let (daemon, events) = browse()?;
    let deadline = std::time::Instant::now() + timeout;
    let mut found: Vec<Peer> = Vec::new();

    while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
        match events.recv_timeout(left) {
            Ok(Discovery::Found(peer)) => {
                if !found.iter().any(|seen| seen.id == peer.id) {
                    found.push(peer);
                }
            }
            Ok(Discovery::Lost(_)) => continue,
            Err(_) => break,
        }
    }

    let _ = daemon.shutdown();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(found)
}

fn peer_of(service: &ResolvedService) -> Peer {
    let txt = |key: &str| {
        service
            .txt_properties
            .get_property_val_str(key)
            .unwrap_or_default()
            .to_string()
    };

    // The id falls back to the mDNS instance name. An advertisement without one
    // is either something else on this service type or a version older than
    // this field, and a peer that cannot be identified is better shown with a
    // weak identity than hidden -- the operator can still see it and pick it.
    let id = {
        let stated = txt(TXT_ID);
        if stated.is_empty() { service.fullname.clone() } else { stated }
    };

    let name = {
        let stated = txt(TXT_NAME);
        if stated.is_empty() { service.host.trim_end_matches('.').to_string() } else { stated }
    };

    Peer {
        id,
        name,
        product: txt(TXT_PRODUCT),
        version: txt(TXT_VERSION),
        // The scope is dropped along with the address it qualifies. Only IPv4
        // is ever connected to (see `Peer::address`), and a zone index that
        // survived into a stored pairing would be a peer that resolves today
        // and not after a reboot renumbers the interfaces.
        addresses: service.addresses.iter().map(|scoped| scoped.to_ip_addr()).collect(),
        port: service.port,
    }
}

/// A DNS-SD instance label from an installation id.
///
/// Instance names live in a DNS record and cannot hold arbitrary bytes, so this
/// keeps what is safe and drops the rest. It is not reversible and does not
/// need to be: the id travels in the TXT record, and this is only a label.
fn instance_name(id: &str) -> String {
    let cleaned: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(48)
        .collect();

    if cleaned.is_empty() {
        "castavox".into()
    } else {
        format!("castavox-{cleaned}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn an_instance_name_survives_whatever_the_id_is() {
        // Ids are made by us and are tame, but this label goes into a DNS
        // record and a stray character there is a registration that fails at
        // runtime on one machine and not another.
        assert_eq!(instance_name("abc-123"), "castavox-abc-123");
        assert_eq!(instance_name("a b/c.d"), "castavox-abcd");
        assert_eq!(instance_name(""), "castavox");
        assert_eq!(instance_name("!!!"), "castavox");
        assert!(instance_name(&"x".repeat(200)).len() < 64, "must fit a DNS label");
    }

    #[test]
    fn ipv4_is_preferred_when_both_are_offered() {
        // Not on the merits. An IPv6 link-local address carries a zone index
        // that has to be right, which is one more thing to get wrong on a
        // network nobody administers, for nothing a church would notice.
        let peer = Peer {
            id: "one".into(),
            name: "Desk".into(),
            product: "pulpitry".into(),
            version: "0.4.0".into(),
            addresses: vec![
                "fe80::1".parse().unwrap(),
                IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20)),
            ],
            port: 7854,
        };
        assert_eq!(peer.address(), Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20))));
    }

    #[test]
    fn a_peer_with_no_address_is_not_offered_as_connectable() {
        let peer = Peer {
            id: "one".into(),
            name: "Desk".into(),
            product: "pulpitry".into(),
            version: "0.4.0".into(),
            addresses: Vec::new(),
            port: 7854,
        };
        assert_eq!(peer.address(), None);
    }

    /// Advertising and finding, over real multicast on this machine.
    ///
    /// Ignored by default: it needs a network interface that carries multicast,
    /// which a CI container often does not, and it is slow by nature -- mDNS
    /// answers when it answers. Run it by hand when this module changes:
    ///
    ///     cargo test --lib mirror -- --ignored --nocapture
    #[test]
    #[ignore = "needs a multicast-capable network"]
    fn advertises_and_is_found() {
        let advert = advertise("test-desk-1", "The Desk", "pulpitry", "0.4.0", 7854)
            .expect("should advertise");

        let found = find(Duration::from_secs(6)).expect("should browse");
        let ours = found
            .iter()
            .find(|peer| peer.id == "test-desk-1")
            .expect("should have found our own advertisement");

        assert_eq!(ours.name, "The Desk");
        assert_eq!(ours.product, "pulpitry");
        assert_eq!(ours.port, 7854);
        assert!(ours.address().is_some(), "should have resolved an address");

        drop(advert);
    }
}
