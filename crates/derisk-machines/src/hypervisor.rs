use std::{sync::Once, time::Duration};

use base64::{Engine, engine::general_purpose::STANDARD};
use crossbeam_channel::{Receiver, Sender, select};
use serde_json::{Value, json};
use virt::{connect::Connect, domain::Domain, error::ErrorNumber};

use crate::{Error, Machine, Result, State, URI};

/// How long a guest-agent call may take before libvirt gives up on it. An
/// agent that is up answers in milliseconds; one that is not (the machine is
/// still booting) would otherwise hold the caller for libvirt's default of
/// several minutes.
const AGENT_TIMEOUT_SECONDS: i32 = 5;

/// A connection to libvirt's system daemon.
///
/// [`Hypervisor::open_read_only`] needs no permission at all, which is
/// enough to list machines and find one by its vsock address.
/// [`Hypervisor::open`] can also start and stop them and run programs in
/// them; polkit lets administrators open it (LosOS's rule) and asks
/// everyone else for an administrator's password.
pub struct Hypervisor {
    connection: Connect,
}

impl Hypervisor {
    /// A connection that can only look.
    pub fn open_read_only() -> Result<Hypervisor> {
        quiet_libvirt();
        Ok(Hypervisor {
            connection: Connect::open_read_only(Some(URI)).map_err(libvirt)?,
        })
    }

    /// A connection that can also change things.
    pub fn open() -> Result<Hypervisor> {
        quiet_libvirt();
        Ok(Hypervisor {
            connection: Connect::open(Some(URI)).map_err(libvirt)?,
        })
    }

    /// Every machine libvirt keeps a definition of, and any it runs without
    /// one, in libvirt's order.
    ///
    /// A machine whose definition cannot be read is left out and logged:
    /// one odd domain made by hand should not empty the desktop's list.
    pub fn machines(&self) -> Result<impl Iterator<Item = Machine>> {
        let domains = self.connection.list_all_domains(0).map_err(libvirt)?;
        Ok(domains.into_iter().filter_map(|domain| {
            describe(&domain)
                .inspect_err(|error| tracing::warn!(%error, "skipping a machine"))
                .ok()
        }))
    }

    /// The machine called `name`.
    pub fn machine(&self, name: &str) -> Result<Machine> {
        describe(&self.domain(name)?)
    }

    /// The running machine at vsock address `cid`, if any.
    ///
    /// The address comes from the kernel's vhost-vsock, which stamps every
    /// connection with the address libvirt gave the sending machine; the
    /// guest has no say in it. So the answer is which machine really sent a
    /// connection, which is what lets the desktop label its windows.
    pub fn machine_at(&self, cid: u32) -> Result<Option<Machine>> {
        Ok(self.machines()?.find(|machine| machine.cid == Some(cid)))
    }

    /// Starts the machine. Starting one that runs already does nothing.
    pub fn start(&self, name: &str) -> Result<()> {
        let domain = self.domain(name)?;
        if domain.is_active().map_err(libvirt)? {
            return Ok(());
        }
        domain.create().map(drop).map_err(libvirt)
    }

    /// Asks the machine to shut down, the way a power button does, and
    /// returns without waiting for it to. The guest may take its time or
    /// refuse; [`Hypervisor::force_off`] does not ask.
    pub fn shut_down(&self, name: &str) -> Result<()> {
        let domain = self.domain(name)?;
        if !domain.is_active().map_err(libvirt)? {
            return Ok(());
        }
        domain.shutdown().map(drop).map_err(libvirt)
    }

    /// Stops the machine at once, as pulling its plug would. What it had
    /// not written to its disk is lost.
    pub fn force_off(&self, name: &str) -> Result<()> {
        let domain = self.domain(name)?;
        if !domain.is_active().map_err(libvirt)? {
            return Ok(());
        }
        domain.destroy().map_err(libvirt)
    }

    /// Whether the machine's guest agent answers yet. A machine that has
    /// just started cannot run programs until it does.
    pub fn agent_ready(&self, name: &str) -> Result<bool> {
        let domain = self.domain(name)?;
        match domain.qemu_agent_command(r#"{"execute":"guest-ping"}"#, AGENT_TIMEOUT_SECONDS, 0) {
            Ok(_) => Ok(true),
            Err(error)
                if matches!(
                    error.code(),
                    ErrorNumber::AgentUnresponsive | ErrorNumber::OperationInvalid
                ) =>
            {
                Ok(false)
            }
            Err(error) => Err(libvirt(error)),
        }
    }

    /// Starts `program` in the machine through its guest agent and returns
    /// the guest's process id for it, without waiting for it to finish.
    ///
    /// The agent runs it as root; LosOS's guest template has
    /// `losos-guest-run` to drop to the guest's user and put the program's
    /// windows on the host.
    pub fn spawn(&self, name: &str, program: &Program) -> Result<u32> {
        let mut arguments = json!({
            "path": program.path,
            "arg": program.args,
            "env": program.env,
            "capture-output": program.capture_output,
        });
        if !program.input.is_empty() {
            arguments["input-data"] = STANDARD.encode(&program.input).into();
        }
        let reply = self.agent(
            name,
            json!({ "execute": "guest-exec", "arguments": arguments }),
        )?;
        reply["pid"]
            .as_u64()
            .and_then(|pid| u32::try_from(pid).ok())
            .ok_or_else(|| Error::Agent {
                name: name.into(),
                reply: reply.to_string(),
            })
    }

    /// How the program [`Hypervisor::spawn`] started as `pid` ended, or
    /// `None` while it still runs.
    pub fn exit(&self, name: &str, pid: u32) -> Result<Option<Exit>> {
        let reply = self.agent(
            name,
            json!({ "execute": "guest-exec-status", "arguments": { "pid": pid } }),
        )?;
        Exit::from_status(&reply).map_err(|()| Error::Agent {
            name: name.into(),
            reply: reply.to_string(),
        })
    }

    fn agent(&self, name: &str, command: Value) -> Result<Value> {
        let domain = self.domain(name)?;
        let reply = domain
            .qemu_agent_command(&command.to_string(), AGENT_TIMEOUT_SECONDS, 0)
            .map_err(libvirt)?;
        let mut reply: Value = serde_json::from_str(&reply).map_err(|_| Error::Agent {
            name: name.into(),
            reply: reply.clone(),
        })?;
        Ok(reply["return"].take())
    }

    fn domain(&self, name: &str) -> Result<Domain> {
        Domain::lookup_by_name(&self.connection, name).map_err(|error| {
            if error.code() == ErrorNumber::NoDomain {
                Error::NoMachine(name.into())
            } else {
                libvirt(error)
            }
        })
    }
}

/// A program to run in a machine.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Program {
    /// Absolute path of the program in the guest. The agent's own `PATH`
    /// is not the guest user's, so a bare name finds whatever it finds.
    pub path: String,
    /// Its arguments, without the program itself.
    pub args: Vec<String>,
    /// `NAME=value` pairs for its environment, which the agent otherwise
    /// gives it empty.
    pub env: Vec<String>,
    /// Keep its output for [`Exit::stdout`] and [`Exit::stderr`]. An app
    /// that runs for hours should not have its output kept in the agent.
    pub capture_output: bool,
    /// What it reads on its standard input; it reads nothing when empty.
    pub input: Vec<u8>,
}

/// How a program run in a machine ended.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Exit {
    /// Its exit code, when it exited.
    pub code: Option<i32>,
    /// The signal that killed it, when one did.
    pub signal: Option<i32>,
    /// What it wrote to its standard output, when that was captured.
    pub stdout: Vec<u8>,
    /// What it wrote to its standard error, when that was captured.
    pub stderr: Vec<u8>,
}

impl Exit {
    /// Whether it exited with code 0.
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    /// From the agent's `guest-exec-status` reply.
    fn from_status(status: &Value) -> Result<Option<Exit>, ()> {
        if !status["exited"].as_bool().ok_or(())? {
            return Ok(None);
        }
        let number = |key: &str| status[key].as_i64().and_then(|n| i32::try_from(n).ok());
        let data = |key: &str| match status[key].as_str() {
            Some(encoded) => STANDARD.decode(encoded).map_err(drop),
            None => Ok(Vec::new()),
        };
        Ok(Some(Exit {
            code: number("exitcode"),
            signal: number("signal"),
            stdout: data("out-data")?,
            stderr: data("err-data")?,
        }))
    }
}

/// Sends the list of machines on `changes` whenever it differs from the
/// last one sent, checking every `every`, until `stop` receives or closes
/// or nobody listens on `changes` any more.
///
/// The first list goes out at once. libvirt can push lifecycle events
/// instead, but only into an event loop this process would have to run for
/// it; a look every second or two costs one round trip on a local socket.
/// Run it on a thread of the caller's: it blocks.
pub fn watch(
    hypervisor: &Hypervisor,
    every: Duration,
    changes: &Sender<Vec<Machine>>,
    stop: &Receiver<()>,
) -> Result<()> {
    let mut last = None;
    loop {
        let machines: Vec<Machine> = hypervisor.machines()?.collect();
        if last.as_ref() != Some(&machines) {
            last = Some(machines.clone());
            select! {
                send(changes, machines) -> sent => if sent.is_err() { return Ok(()) },
                recv(stop) -> _ => return Ok(()),
            }
        }
        select! {
            recv(stop) -> _ => return Ok(()),
            default(every) => {}
        }
    }
}

fn describe(domain: &Domain) -> Result<Machine> {
    let (state, _reason) = domain.get_state().map_err(libvirt)?;
    let xml = domain.get_xml_desc(0).map_err(libvirt)?;
    Machine::from_xml(&xml, State::from_libvirt(state))
}

fn libvirt(error: virt::error::Error) -> Error {
    Error::Libvirt(error.message().to_owned())
}

/// libvirt prints every error to standard error unless told not to. derisk
/// reports them itself, as diagnostics, where a person sees them.
fn quiet_libvirt() {
    static ONCE: Once = Once::new();
    ONCE.call_once(virt::error::clear_error_callback);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_status() {
        assert_eq!(Exit::from_status(&json!({ "exited": false })), Ok(None));
        let exit = Exit::from_status(&json!({
            "exited": true,
            "exitcode": 0,
            "out-data": STANDARD.encode("org.derisk.files\n"),
        }))
        .unwrap()
        .unwrap();
        assert!(exit.success());
        assert_eq!(exit.stdout, b"org.derisk.files\n");
        assert!(exit.stderr.is_empty());

        let killed = Exit::from_status(&json!({ "exited": true, "signal": 9 }))
            .unwrap()
            .unwrap();
        assert!(!killed.success());
        assert_eq!(killed.signal, Some(9));

        assert!(Exit::from_status(&json!({})).is_err());
        assert!(Exit::from_status(&json!({ "exited": true, "out-data": "%%" })).is_err());
    }

    /// Against the libvirt daemon of the machine running the tests, which
    /// CI does not have. `cargo test -p derisk-machines -- --ignored`.
    #[test]
    #[ignore = "needs libvirtd"]
    fn lists_machines() {
        let hypervisor = Hypervisor::open_read_only().unwrap();
        for machine in hypervisor.machines().unwrap() {
            assert_eq!(
                hypervisor.machine(&machine.name).unwrap().uuid,
                machine.uuid
            );
        }
        assert!(matches!(
            hypervisor.machine("no-such-machine"),
            Err(Error::NoMachine(_))
        ));
    }
}
