//
// Copyright (C) 2018 Kubos Corporation
//
// Licensed under the Apache License, Version 2.0 (the "License")
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//

use failure::Fail;
use mai400_api::*;
use std::cell::{Cell, RefCell};
use std::io::{Error, ErrorKind};
use std::sync::{Arc, Mutex};
use std::thread::{spawn, sleep};
use std::time::Duration;

use objects::*;

pub struct ReadData {
    pub std_telem: Mutex<StandardTelemetry>,
    pub irehs_telem: Mutex<IREHSTelemetry>,
    pub imu: Mutex<RawIMU>,
    pub rotating: Mutex<RotatingTelemetry>,
}

impl ReadData {
    pub fn new() -> ReadData {
        ReadData {
            std_telem: Mutex::new(StandardTelemetry::default()),
            irehs_telem: Mutex::new(IREHSTelemetry::default()),
            imu: Mutex::new(RawIMU::default()),
            rotating: Mutex::new(RotatingTelemetry::default()),
        }
    }

    pub fn update_std(&self, telem: StandardTelemetry) {
        {
            let mut local = self.std_telem.lock().unwrap();
            *local = telem.clone();
        }

        let mut local = self.rotating.lock().unwrap();
        local.update(&telem);
    }

    pub fn update_irehs(&self, irehs: IREHSTelemetry) {
        let mut local = self.irehs_telem.lock().unwrap();
        *local = irehs;
    }

    pub fn update_imu(&self, imu: RawIMU) {
        let mut local = self.imu.lock().unwrap();
        *local = imu;
    }
}

pub fn read_thread(bus: String, data: Arc<ReadData>) -> MAIResult<()> {
    let connection = Connection::new(bus);
    let mai = MAI400::new(connection);

    loop {
        // TODO: Error handling and reporting
        let (std, imu, irehs) = mai.get_message().unwrap();

        if let Some(telem) = std {
            data.update_std(telem);
        }
        if let Some(telem) = imu {
            data.update_imu(telem);
        }
        if let Some(telem) = irehs {
            data.update_irehs(telem);
        }
    }
}

pub struct Subsystem {
    pub mai: MAI400,
    pub last_cmd: Cell<AckCommand>,
    pub errors: RefCell<Vec<String>>,
    pub persistent: Arc<ReadData>,
}

impl Subsystem {
    pub fn new(bus: String, data: Arc<ReadData>) -> Subsystem {
        println!("New Subsystem");

        let connection = Connection::new(bus.clone());
        let mai = MAI400::new(connection);

        let data_ref = data.clone();

        spawn(move || read_thread(bus, data_ref));

        Subsystem {
            mai,
            last_cmd: Cell::new(AckCommand::None),
            errors: RefCell::new(vec![]),
            persistent: data.clone(),
        }
    }

    // Queries

    pub fn get_power(&self) -> Result<GetPowerResponse, Error> {
        let old_ctr = {
            self.persistent.std_telem.lock().unwrap().tlm_counter
        };

        // Wait long enough for a new telemetry set to be read
        sleep(Duration::from_millis(300));

        let new_ctr = {
            self.persistent.std_telem.lock().unwrap().tlm_counter
        };

        let (state, uptime) = match new_ctr != old_ctr {
            true => (
                PowerState::On,
                self.persistent.std_telem.lock().unwrap().cmd_valid_cntr as i32,
            ),
            false => (PowerState::Off, 0),
        };

        Ok(GetPowerResponse { state, uptime })
    }

    pub fn get_telemetry(&self) -> Result<Telemetry, Error> {
        Ok(Telemetry {
            nominal: StdTelem(self.persistent.std_telem.lock().unwrap().clone()),
            debug: TelemetryDebug {
                irehs: IREHSTelem(self.persistent.irehs_telem.lock().unwrap().clone()),
                raw_imu: RawIMUTelem(self.persistent.imu.lock().unwrap().clone()),
                rotating: Rotating(self.persistent.rotating.lock().unwrap().clone()),
            },
        })
    }

    pub fn get_test_results(&self) -> Result<IntegrationTestResults, Error> {
        Ok(IntegrationTestResults {
            success: true,
            errors: "".to_owned(),
            telemetry_nominal: StdTelem(self.persistent.std_telem.lock().unwrap().clone()),
            telemetry_debug: TelemetryDebug {
                irehs: IREHSTelem(self.persistent.irehs_telem.lock().unwrap().clone()),
                raw_imu: RawIMUTelem(self.persistent.imu.lock().unwrap().clone()),
                rotating: Rotating(self.persistent.rotating.lock().unwrap().clone()),
            },
        })
    }

    pub fn get_mode(&self) -> Result<Mode, Error> {
        let raw = match self.persistent.std_telem.lock() {
            Ok(telem) => telem.acs_mode,
            _ => 0xFF,
        };

        Ok(Mode::from(raw))
    }

    pub fn get_spin(&self) -> Result<Spin, Error> {
        let rotating = self.persistent.rotating.lock().unwrap();
        Ok(Spin {
            x: rotating.k_bdot[0] as f64,
            y: rotating.k_bdot[1] as f64,
            z: rotating.k_bdot[2] as f64,
        })
    }

    // Mutations

    pub fn noop(&self) -> Result<GenericResponse, Error> {
        let old_ctr = {
            self.persistent.std_telem.lock().unwrap().tlm_counter
        };

        // Wait long enough for a new telemetry set to be read
        sleep(Duration::from_millis(300));

        let new_ctr = {
            self.persistent.std_telem.lock().unwrap().tlm_counter
        };

        let (success, errors) = match new_ctr != old_ctr {
            true => (true, "".to_owned()),
            false => {
                push_err!(
                    self.errors,
                    "Noop: Unable to communicate with MAI400".to_owned()
                );
                (false, "Unable to communicate with MAI400".to_owned())
            }
        };

        Ok(GenericResponse { success, errors })
    }

    pub fn control_power(&self, state: PowerState) -> Result<ControlPowerResponse, Error> {
        match state {
            PowerState::Reset => {
                let result = run!(self.mai.reset(), self.errors);

                Ok(ControlPowerResponse {
                    power: state,
                    success: result.is_ok(),
                    errors: match result {
                        Ok(_) => "".to_owned(),
                        Err(err) => err,
                    },
                })

            } 
            _ => {
                push_err!(self.errors, "controlPower: Invalid power state".to_owned());

                Ok(ControlPowerResponse {
                    power: state,
                    errors: String::from("Invalid power state"),
                    success: false,
                })
            }

        }
    }

    pub fn passthrough(&self, command: String) -> Result<GenericResponse, Error> {
        // Convert the hex values in the string into actual hex values
        // Ex. "c3c2" -> [0xc3, 0xc2]
        let tx: Vec<u8> = command
            .as_bytes()
            .chunks(2)
            .into_iter()
            .map(|chunk| {
                u8::from_str_radix(::std::str::from_utf8(chunk).unwrap(), 16).unwrap()
            })
            .collect();

        let result = run!(self.mai.passthrough(tx.as_slice()), self.errors);

        Ok(GenericResponse {
            success: result.is_ok(),
            errors: match result {
                Ok(_) => "".to_owned(),
                Err(err) => err,
            },
        })
    }

    pub fn set_mode(&self, mode: u8, qbi_cmd: Vec<i32>) -> Result<GenericResponse, Error> {
        if qbi_cmd.len() != 4 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "qbi_cmd must contain exactly 4 elements",
            ));
        }

        let result = run!(
            self.mai.set_mode(
                mode,
                [
                    qbi_cmd[0] as i16,
                    qbi_cmd[1] as i16,
                    qbi_cmd[2] as i16,
                    qbi_cmd[3] as i16,
                ],
            ),
            self.errors
        );

        Ok(GenericResponse {
            success: result.is_ok(),
            errors: match result {
                Ok(_) => "".to_owned(),
                Err(err) => err,
            },
        })
    }

    pub fn set_mode_sun(
        &self,
        mode: u8,
        sun_angle_enable: i16,
        sun_rot_angle: f32,
    ) -> Result<GenericResponse, Error> {

        let result = run!(
            self.mai.set_mode_sun(mode, sun_angle_enable, sun_rot_angle),
            self.errors
        );

        Ok(GenericResponse {
            success: result.is_ok(),
            errors: match result {
                Ok(_) => "".to_owned(),
                Err(err) => err,
            },
        })
    }

    pub fn update(
        &self,
        gps_time: Option<i32>,
        rv: Option<RVInput>,
    ) -> Result<GenericResponse, Error> {
        let mut success = true;
        let mut errors = "".to_owned();

        if let Some(time) = gps_time {
            let result = run!(self.mai.set_gps_time(time as u32), self.errors);
            success &= result.is_ok();
            if let Err(err) = result {
                errors.push_str(&err);
            }
        }

        if let Some(params) = rv {
            if params.eci_pos.len() != 3 {
                //TODO: throw better error
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "eci_pos must contain exactly 3 elements",
                ));
            }

            if params.eci_vel.len() != 3 {
                //TODO: throw better error
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "eci_vel must contain exactly 3 elements",
                ));
            }

            let result = run!(
                self.mai.set_rv(
                    [
                        params.eci_pos[0] as f32,
                        params.eci_pos[1] as f32,
                        params.eci_pos[2] as f32,
                    ],
                    [
                        params.eci_vel[0] as f32,
                        params.eci_vel[1] as f32,
                        params.eci_vel[2] as f32,
                    ],
                    params.time_epoch as u32,
                ),
                self.errors
            );
            success &= result.is_ok();
            if let Err(err) = result {
                errors.push_str(&err);
            }
        }

        Ok(GenericResponse { success, errors })
    }
}
