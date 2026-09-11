// Fixture for agentx-try-wait-outside-finish. Contains rule-violating code on purpose.

impl IpCommand {
    fn finish(&mut self) -> Result<ExitStatus> {
        let mut child = self.child.take().expect("unreaped ip child");
        kill_group(child.id());
        // ok: agentx-try-wait-outside-finish
        match child.try_wait() {
            Ok(Some(status)) => Ok(status),
            _ => Err(Error::other("not reaped")),
        }
    }

    fn reap_early(&mut self) -> Result<()> {
        let child = self.child.as_mut().expect("unreaped ip child");
        // ruleid: agentx-try-wait-outside-finish
        let _ = child.try_wait()?;
        Ok(())
    }
}

fn wait_bounded(child: &mut Child) -> Result<ExitStatus> {
    loop {
        // ruleid: agentx-try-wait-outside-finish
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
    }
}

// A method with the same signature in another type must not inherit the exemption.
impl SomethingElse {
    fn finish(&mut self) -> Result<ExitStatus> {
        let mut child = self.child.take().expect("unreaped child");
        // ruleid: agentx-try-wait-outside-finish
        match child.try_wait() {
            Ok(Some(status)) => Ok(status),
            _ => Err(Error::other("not reaped")),
        }
    }
}
