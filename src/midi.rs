use midir::{Ignore, MidiInput, MidiOutput, MidiInputPort, MidiOutputPort};
use std::error::Error;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

pub enum MidiEvent {
    NoteOn { note: u8, velocity: u8 },
    ControlChange { controller: u8, value: u8 },
    Connected,
    Disconnected,
}

pub struct MidiConnectionPayload {
    pub midi_in: MidiInput,
    pub midi_out: MidiOutput,
    pub in_port: MidiInputPort,
    pub out_port: MidiOutputPort,
}

pub enum MidiCommand {
    SetPadColor { note: u8, color: u8 },
    SetButtonColor { cc: u8, color: u8 },
    ClearAll,
    Connect(Box<MidiConnectionPayload>),
    Disconnect,
}

// Detection Function (Runs on Main Thread)
pub fn detect_launchpad() -> Option<MidiConnectionPayload> {
    // Create new instances (Safe to do on Main Thread)
    // Using a more generic name for reuse if needed, or specific to detection
    let mut midi_in = MidiInput::new("Lightspeed Input").ok()?;
    midi_in.ignore(Ignore::None);
    let midi_out = MidiOutput::new("Lightspeed Output").ok()?;

    let in_ports = midi_in.ports();
    let out_ports = midi_out.ports();

    // Find Input - STRICT: only use ports with valid, readable names
    // 1. Prefer "Launchpad" AND "MIDI"
    // 2. Prefer "Launchpad" AND NOT "DAW"
    // 3. Fallback to any "Launchpad"

    let lp_in = in_ports.iter().find(|p| {
        let Ok(name) = midi_in.port_name(p) else { return false; };
        name.contains("Launchpad") && (name.contains("MIDI") || name.contains("LPMiniMK3 MIDI"))
    }).or_else(|| {
        in_ports.iter().find(|p| {
            let Ok(name) = midi_in.port_name(p) else { return false; };
            name.contains("Launchpad") && !name.contains("DAW")
        })
    }).or_else(|| {
        in_ports.iter().find(|p| {
            let Ok(name) = midi_in.port_name(p) else { return false; };
            name.contains("Launchpad")
        })
    });

    let lp_out = out_ports.iter().find(|p| {
        let Ok(name) = midi_out.port_name(p) else { return false; };
        name.contains("Launchpad") && (name.contains("MIDI") || name.contains("LPMiniMK3 MIDI"))
    }).or_else(|| {
        out_ports.iter().find(|p| {
            let Ok(name) = midi_out.port_name(p) else { return false; };
            name.contains("Launchpad") && !name.contains("DAW")
        })
    }).or_else(|| {
        out_ports.iter().find(|p| {
            let Ok(name) = midi_out.port_name(p) else { return false; };
            name.contains("Launchpad")
        })
    });

    if let (Some(in_port), Some(out_port)) = (lp_in, lp_out) {
        // Clone ports because we need to move them into the payload
        // MidiPort is usually Clone, let's check. Yes, likely thin wrapper.
        // If MidiPort isn't Clone, we'd have to use index, but midir ports are opaque structs.
        // Checking docs or assumption: MidiPort usually implements Clone.
        // If not, we have a problem because iter returns references.
        // But midir::MidiPort IS Clone.
        return Some(MidiConnectionPayload {
            midi_in,
            midi_out,
            in_port: in_port.clone(),
            out_port: out_port.clone(),
        });
    }

    None
}

pub fn start_midi_service(tx_to_app: Sender<MidiEvent>) -> Sender<MidiCommand> {
    let (tx_cmd, rx_cmd) = std::sync::mpsc::channel();

    thread::spawn(move || {
        println!("MIDI Background Service Started");
        
        loop {
            // Wait for a Connect command
            // We can block here because we have nothing else to do until we connect
            match rx_cmd.recv() {
                Ok(MidiCommand::Connect(payload)) => {
                    println!("Received MIDI connection payload. Connecting...");
                    
                    // Unbox and run the loop with the PRE-EXISTING instances
                    let res = run_midi_loop(
                        &tx_to_app, 
                        &rx_cmd, 
                        *payload 
                    );
                    
                    if let Err(e) = res {
                        println!("MIDI Loop ended with error: {:?}", e);
                        let _ = tx_to_app.send(MidiEvent::Disconnected);
                    }
                    
                    // After disconnect/error, go back to top of loop waiting for new connection
                },
                Ok(_) => {
                    // Ignore other commands while disconnected
                },
                Err(_) => break, // Channel closed
            }
        }
    });

    tx_cmd
}

fn run_midi_loop(
    tx_event: &Sender<MidiEvent>,
    rx_cmd: &Receiver<MidiCommand>,
    payload: MidiConnectionPayload,
) -> Result<(), Box<dyn Error>> {
    
    // Deconstruct the payload
    let MidiConnectionPayload { midi_in, midi_out, in_port, out_port } = payload;

    let in_name = midi_in.port_name(&in_port).unwrap_or_else(|_| "Unknown".to_string());
    let out_name = midi_out.port_name(&out_port).unwrap_or_else(|_| "Unknown".to_string());
    println!("Connecting to Launched Ports: In={}, Out={}", in_name, out_name);

    let tx = tx_event.clone();

    // Connect using the instances passed from Main Thread
    let _conn_in = midi_in.connect(
        &in_port,
        "launchpad-in",
        move |_stamp, message, _| {
            if message.len() >= 3 {
                let status = message[0] & 0xF0;
                match status {
                    0x90 => {
                        let note = message[1];
                        let vel = message[2];
                        if vel > 0 {
                            let _ = tx.send(MidiEvent::NoteOn { note, velocity: vel });
                        }
                    }
                    0xB0 => {
                        let cc = message[1];
                        let val = message[2];
                        if val > 0 {
                            let _ = tx.send(MidiEvent::ControlChange {
                                controller: cc,
                                value: val,
                            });
                        }
                    }
                    _ => {}
                }
            }
        },
        (),
    ).map_err(|e| format!("Failed to connect input: {}", e))?;

    let mut conn_out = midi_out.connect(&out_port, "launchpad-out")
        .map_err(|e| format!("Failed to connect output: {}", e))?;

    // === CRITICAL HANDSHAKE ===
    thread::sleep(Duration::from_millis(200)); 
    
    // Enter Programmer Mode
    // F0h 00h 20h 29h 02h 0Dh 0Eh 01h F7h
    let sysex = &[0xF0, 0x00, 0x20, 0x29, 0x02, 0x0D, 0x0E, 0x01, 0xF7];
    conn_out.send(sysex)?;
    
    println!("Launchpad Programmer Mode Enabled");
    
    thread::sleep(Duration::from_millis(200)); // WAIT for mode switch
    
    // Now send connected event
    let _ = tx_event.send(MidiEvent::Connected);

    println!("Device Ready. Listening for commands...");

    // Process Commands
    loop {
        match rx_cmd.recv_timeout(Duration::from_secs(1)) {
            Ok(cmd) => match cmd {
                MidiCommand::SetPadColor { note, color } => {
                    conn_out.send(&[0x90, note, color])?; 
                },
                MidiCommand::SetButtonColor { cc, color } => {
                     conn_out.send(&[0xB0, cc, color])?; 
                },
                MidiCommand::ClearAll => {
                    for i in 0..127 {
                         conn_out.send(&[0x90, i, 0])?;
                         conn_out.send(&[0xB0, i, 0])?;
                    }
                },
                MidiCommand::Connect(_) => {
                    println!("Received Connect command while already connected. Ignoring.");
                },
                MidiCommand::Disconnect => {
                    println!("Disconnect requested by Watchdog.");
                    break;
                }
            },
            Err(RecvTimeoutError::Timeout) => {
                // Heartbeat: Send a dummy message to check connection health
                // Note Off on Channel 1, Note 0, Velocity 0
                conn_out.send(&[0x80, 0, 0])?;
            },
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    Ok(())
}
