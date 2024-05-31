use crate::mapping::*;
use anyhow::*;
use evdev_rs::{DeviceWrapper, Device, GrabMode, InputEvent, ReadFlag, TimeVal, UInputDevice};
use evdev_rs::enums::EV_KEY;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

#[derive(Clone, Copy, Debug)]
enum KeyEventType {
    Release,
    Press,
    Repeat,
    Unknown(i32),
}

impl KeyEventType {
    fn from_value(value: i32) -> Self {
        match value {
            0 => KeyEventType::Release,
            1 => KeyEventType::Press,
            2 => KeyEventType::Repeat,
            _ => KeyEventType::Unknown(value),
        }
    }

    fn value(&self) -> i32 {
        match self {
            Self::Release => 0,
            Self::Press => 1,
            Self::Repeat => 2,
            Self::Unknown(n) => *n,
        }
    }
}

fn timeval_diff(newer: &TimeVal, older: &TimeVal) -> Duration {
    const MICROS_PER_SECOND: libc::time_t = 1000000;
    let secs = newer.tv_sec - older.tv_sec;
    let usecs = newer.tv_usec - older.tv_usec;

    let (secs, usecs) = if usecs < 0 {
        (secs - 1, usecs + MICROS_PER_SECOND)
    } else {
        (secs, usecs)
    };

    Duration::from_micros(((secs * MICROS_PER_SECOND) + usecs) as u64)
}

const fn to_event_type(code: &EventCode) -> EventType {
    match code {
        EventCode::EV_SYN(_n) => EventType::EV_SYN,
        EventCode::EV_KEY(_n) => EventType::EV_KEY,
        EventCode::EV_REL(_n) => EventType::EV_REL,
        EventCode::EV_ABS(_n) => EventType::EV_ABS,
        EventCode::EV_MSC(_n) => EventType::EV_MSC,
        EventCode::EV_SW(_n) => EventType::EV_SW,
        EventCode::EV_LED(_n) => EventType::EV_LED,
        EventCode::EV_SND(_n) => EventType::EV_SND,
        EventCode::EV_REP(_n) => EventType::EV_REP,
        EventCode::EV_FF(_n) => EventType::EV_FF,
        EventCode::EV_PWR => EventType::EV_PWR,
        EventCode::EV_FF_STATUS(_n) => EventType::EV_FF_STATUS,
        EventCode::EV_MAX => EventType::EV_MAX,
        _ => EventType::EV_UNK,
    }
}

pub struct InputMapper {
    input: Device,
    output: UInputDevice,
    /// If present in this map, the key is down since the instant
    /// of its associated value
    input_state: HashMap<KeyCode, TimeVal>,

    mappings: Vec<Mapping>,
    mapped_types: HashSet<EventType>,

    /// The most recent candidate for a tap function is held here
    tapping: Option<KeyCode>,

    output_keys: HashSet<KeyCode>,
}

fn enable_key_code(input: &mut Device, key: KeyCode) -> Result<()> {
    input
        .enable(key.clone())
        .context(format!("enable key {:?}", key))?;
    Ok(())
}

impl InputMapper {
    pub fn create_mapper<P: AsRef<Path>>(path: P, mappings: Vec<Mapping>) -> Result<Self> {
        let path = path.as_ref();
        let f = std::fs::File::open(path).context(format!("opening {}", path.display()))?;
        let mut input_device = Device::new_from_file(f)
            .with_context(|| format!("failed to create new Device from file {}", path.display()))?;

        input_device.set_name(&format!("evremap Virtual input for {}", path.display()));
        let mut mapped_types = HashSet::new();
        // Ensure that any remapped keys are supported by the generated output device
        for map in &mappings {
            match map {
                Mapping::DualRole {input, tap, hold, .. } => {
                    mapped_types.insert(to_event_type(input));
                    for t in tap {
                        enable_key_code(&mut input_device, t.clone())?;
                    }
                    for h in hold {
                        enable_key_code(&mut input_device, h.clone())?;
                    }
                }
                Mapping::Remap { input, output, .. } => {
                    for i in input {
                        mapped_types.insert(to_event_type(&i.code));
                    }
                    for o in output {
                        enable_key_code(&mut input_device, o.code.clone())?;
                    }
                }
            }
        }

        let output = UInputDevice::create_from_device(&input_device)
            .context(format!("creating UInputDevice from {}", path.display()))?;

        input_device
            .grab(GrabMode::Grab)
            .context(format!("grabbing exclusive access on {}", path.display()))?;

        Ok(Self {
            input: input_device,
            output,
            input_state: HashMap::new(),
            output_keys: HashSet::new(),
            tapping: None,
            mappings,
            mapped_types,
        })
    }

    pub fn run_mapper(&mut self) -> Result<()> {
        log::info!("Going into read loop");
        loop {
            let (status, event) = self
                .input
                .next_event(ReadFlag::NORMAL | ReadFlag::BLOCKING)?;
            match status {
                evdev_rs::ReadStatus::Success => {
                    if self.mapped_types.contains(&to_event_type(&event.event_code)) {
                        log::trace!("IN {:?}", event);
                        self.update_with_event(&event, event.event_code.clone())?;
                    } else {
                        log::trace!("PASSTHRU {:?}", event);
                        self.output.write_event(&event)?;
                    }
                }
                evdev_rs::ReadStatus::Sync => bail!("ReadStatus::Sync!"),
            }
        }
    }

    /// Compute the effective set of keys that are pressed
    fn compute_keys(&self) -> HashSet<KeyCode> {
        // Start with the input keys
        let mut keys: HashSet<KeyCode> = self.input_state.keys().cloned().collect();

        // First phase is to apply any DualRole mappings as they are likely to
        // be used to produce modifiers when held.
        for map in &self.mappings {
            if let Mapping::DualRole { input, hold, .. } = map {
                if keys.contains(input) {
                    keys.remove(input);
                    for h in hold {
                        keys.insert(h.clone());
                    }
                }
            }
        }

        let mut keys_minus_remapped = keys.clone();

        // Second pass to apply Remap items
        for map in &self.mappings {
            if let Mapping::Remap { input, output } = map {
                if keys_minus_remapped.is_superset(&input.into_iter().map(|k|k.code).collect()) {
                    for i in input {
                        keys.remove(&i.code);
                        if let EventCode::EV_KEY(k) = i.code {
                            if !is_modifier(&k) {
                                keys_minus_remapped.remove(&i.code);
                            }
                        }
                    }
                    for o in output {
                        keys.insert(o.code.clone());
                        // Outputs that apply are not visible as
                        // inputs for later remap rules
                        if let EventCode::EV_KEY(k) = o.code {
                            if !is_modifier(&k) {
                                keys_minus_remapped.remove(&o.code);
                            }
                        }
                    }
                }
            }
        }

        keys
    }

    /// Compute the difference between our desired set of keys
    /// and the set of keys that are currently pressed in the
    /// output device.
    /// Release any keys that should not be pressed, and then
    /// press any keys that should be pressed.
    ///
    /// When releasing, release modifiers last so that mappings
    /// that produce eg: CTRL-C don't emit a random C character
    /// when released.
    ///
    /// Similarly, when pressing, emit modifiers first so that
    /// we don't emit C and then CTRL for such a mapping.
    fn compute_and_apply_keys(&mut self, time: &TimeVal) -> Result<()> {
        let desired_keys = self.compute_keys();
        let mut to_release: Vec<KeyCode> = self
            .output_keys
            .difference(&desired_keys)
            .cloned()
            .collect();

        let mut to_press: Vec<KeyCode> = desired_keys
            .difference(&self.output_keys)
            .cloned()
            .collect();

        if !to_release.is_empty() {
            to_release.sort_by(modifiers_last);
            self.emit_keys(&to_release, time, KeyEventType::Release)?;
        }
        if !to_press.is_empty() {
            to_press.sort_by(modifiers_first);
            self.emit_keys(&to_press, time, KeyEventType::Press)?;
        }
        Ok(())
    }

    fn lookup_dual_role_mapping(&self, code: KeyCode) -> Option<Mapping> {
        for map in &self.mappings {
            if let Mapping::DualRole { input, .. } = map {
                if *input == code {
                    // A DualRole mapping has the highest precedence
                    // so we've found our match
                    return Some(map.clone());
                }
            }
        }
        None
    }

    fn lookup_mapping(&self, code: KeyCode, value: i32) -> Option<Mapping> {
        let mut candidates = vec![];

        for map in &self.mappings {
            match map {
                Mapping::DualRole { input, .. } => {
                    if *input == code {
                        // A DualRole mapping has the highest precedence
                        // so we've found our match
                        return Some(map.clone());
                    }
                }
                Mapping::Remap { input, .. } => {
                    // Look for a mapping that includes the current key.
                    // If part of a chord, all of its component keys must
                    // also be pressed.
                    let mut code_matched = false;
                    let mut all_matched = true;
                    for i in input {
                        if *i == code {
                            code_matched = match i.code {
                                EventCode::EV_KEY(_) => true,
                                _ => i.scale == 0 || i.scale.is_negative() == value.is_negative()
                            }
                        } else if !self.input_state.contains_key(&i.code) {
                            all_matched = false;
                            break;
                        }
                    }
                    if code_matched && all_matched {
                        candidates.push(map);
                    }
                }
            }
        }

        // Any matches must be Remap entries.  We want the one
        // with the most active keys
        candidates.sort_by(|a, b| match (a, b) {
            (Mapping::Remap { input: input_a, .. }, Mapping::Remap { input: input_b, .. }) => {
                input_a.len().cmp(&input_b.len()).reverse()
            }
            _ => unreachable!(),
        });

        candidates.get(0).map(|&m| m.clone())
    }

    pub fn update_with_event(&mut self, event: &InputEvent, code: KeyCode) -> Result<()> {
        match event.event_type().ok_or("Unknown event type").unwrap() {
            EventType::EV_KEY => {
                let event_type = KeyEventType::from_value(event.value);
                match event_type {
                    KeyEventType::Release => {
                        let pressed_at = match self.input_state.remove(&code) {
                            None => {
                                self.write_event_and_sync(event)?;
                                return Ok(());
                            }
                            Some(p) => p,
                        };

                        self.compute_and_apply_keys(&event.time)?;

                        if let Some(Mapping::DualRole { tap, .. }) =
                            self.lookup_dual_role_mapping(code.clone())
                        {
                            // If released quickly enough, becomes a tap press.
                            if let Some(tapping) = self.tapping.take() {
                                if tapping == code
                                    && timeval_diff(&event.time, &pressed_at) <= Duration::from_millis(200)
                                {
                                    self.emit_keys(&tap, &event.time, KeyEventType::Press)?;
                                    self.emit_keys(&tap, &event.time, KeyEventType::Release)?;
                                }
                            }
                        }
                    }
                    KeyEventType::Press => {
                        self.input_state.insert(code.clone(), event.time.clone());

                        match self.lookup_mapping(code.clone(), KeyEventType::Press.value()) {
                            Some(_) => {
                                self.compute_and_apply_keys(&event.time)?;
                                self.tapping.replace(code);
                            }
                            None => {
                                // Just pass it through
                                self.cancel_pending_tap();
                                self.compute_and_apply_keys(&event.time)?;
                            }
                        }
                    }
                    KeyEventType::Repeat => {
                        match self.lookup_mapping(code.clone(), KeyEventType::Repeat.value()) {
                            Some(Mapping::DualRole { hold, .. }) => {
                                self.emit_keys(&hold, &event.time, KeyEventType::Repeat)?;
                            }
                            Some(Mapping::Remap { output, .. }) => {
                                let output: Vec<KeyCode> = output.into_iter().map(|k|k.code).collect();
                                self.emit_keys(&output, &event.time, KeyEventType::Repeat)?;
                            }
                            None => {
                                // Just pass it through
                                self.cancel_pending_tap();
                                self.write_event_and_sync(event)?;
                            }
                        }
                    }
                    KeyEventType::Unknown(_) => {
                        self.write_event_and_sync(event)?;
                    }
                };
            }
            _ => {  // All other event types, assume provides value
                match self.lookup_mapping(code.clone(), event.value) {
                    Some(Mapping::Remap { input, output, .. }) => {
                        match input.iter().find(|wrapper| wrapper.code == event.event_code && (wrapper.scale == 0 || wrapper.scale.is_negative() == event.value.is_negative())) {
                            Some(input_wrapper) => {
                                for k in output {
                                    let out_val = match k.code {
                                        EventCode::EV_KEY(_) => KeyEventType::Press.value(),
                                        _ => (event.value / (if input_wrapper.scale == 0 {1} else {input_wrapper.scale})) * (if k.scale == 0 {1} else {k.scale}),
                                    };
                                    self.write_event(&InputEvent::new(&event.time, &k.code, out_val)).expect("Failed to write event");
                                    if let EventCode::EV_KEY(_) = k.code {
                                        self.write_event(&InputEvent::new(&event.time, &k.code, KeyEventType::Release.value())).expect("Failed to write event");
                                    }
                                }
                                self.generate_sync_event(&event.time)?;
                            },
                            None => {}
                        }
                    }
                    _ => {
                        // Just pass it through
                        self.cancel_pending_tap();
                        self.write_event_and_sync(event)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn cancel_pending_tap(&mut self) {
        self.tapping.take();
    }

    fn emit_keys(
        &mut self,
        key: &[KeyCode],
        time: &TimeVal,
        event_type: KeyEventType,
    ) -> Result<()> {
        for k in key {
            let event = make_event(k.clone(), time, event_type);
            self.write_event(&event)?;
        }
        self.generate_sync_event(time)?;
        Ok(())
    }

    fn write_event_and_sync(&mut self, event: &InputEvent) -> Result<()> {
        self.write_event(event)?;
        self.generate_sync_event(&event.time)?;
        Ok(())
    }

    fn write_event(&mut self, event: &InputEvent) -> Result<()> {
        log::trace!("OUT: {:?}", event);
        self.output.write_event(&event)?;
        if let EventCode::EV_KEY(_) = event.event_code {
            let event_type = KeyEventType::from_value(event.value);
            match event_type {
                KeyEventType::Press | KeyEventType::Repeat => {
                    self.output_keys.insert(event.event_code.clone());
                }
                KeyEventType::Release => {
                    self.output_keys.remove(&event.event_code);
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn generate_sync_event(&self, time: &TimeVal) -> Result<()> {
        self.output.write_event(&InputEvent::new(
            time,
            &EventCode::EV_SYN(evdev_rs::enums::EV_SYN::SYN_REPORT),
            0,
        ))?;
        Ok(())
    }
}

fn make_event(key: KeyCode, time: &TimeVal, event_type: KeyEventType) -> InputEvent {
    InputEvent::new(time, &key, event_type.value())
}

fn is_modifier(key: &EV_KEY) -> bool {
    match key {
        EV_KEY::KEY_FN
        | EV_KEY::KEY_LEFTALT
        | EV_KEY::KEY_RIGHTALT
        | EV_KEY::KEY_LEFTMETA
        | EV_KEY::KEY_RIGHTMETA
        | EV_KEY::KEY_LEFTCTRL
        | EV_KEY::KEY_RIGHTCTRL
        | EV_KEY::KEY_LEFTSHIFT
        | EV_KEY::KEY_RIGHTSHIFT => true,
        _ => false,
    }
}

/// Orders modifier keys ahead of non-modifier keys.
/// Unfortunately the underlying type doesn't allow direct
/// comparison, but that's ok for our purposes.
fn modifiers_first(a: &KeyCode, b: &KeyCode) -> Ordering {
    let mut a_ismod = false;
    let mut b_ismod = false;
    if let EventCode::EV_KEY(k) = a {
       a_ismod = is_modifier(&k);
    }
    if let EventCode::EV_KEY(k) = b {
       b_ismod = is_modifier(&k);
    }
    if a_ismod && b_ismod {
        if b_ismod {
            Ordering::Equal
        } else {
            Ordering::Less
        }
    } else if b_ismod {
        Ordering::Greater
    } else {
        // Neither are modifiers
        Ordering::Equal
    }
}

fn modifiers_last(a: &KeyCode, b: &KeyCode) -> Ordering {
    modifiers_first(a, b).reverse()
}
