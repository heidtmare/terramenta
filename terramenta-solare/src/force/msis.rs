//! NRLMSISE-00 (Picone et al. 2002) via the `nrlmsise00` crate; `gtd7d` total mass density
//! (anomalous O included), full 3-h Ap history.

use std::sync::Arc;

use hifitime::TimeScale;
use nrlmsise00::{Input, MagneticActivity, Model};

use super::drag::{Atmosphere, AtmosphereSample};
use crate::space_weather::{SpaceWeather, civil_to_mjd};

#[derive(Debug, Clone)]
pub struct Nrlmsise00 {
    pub weather: Arc<SpaceWeather>,
    model: Model,
}

impl Nrlmsise00 {
    pub fn new(weather: Arc<SpaceWeather>) -> Self {
        Self {
            weather,
            model: Model::new(),
        }
    }
}

impl Atmosphere for Nrlmsise00 {
    fn density(&self, s: &AtmosphereSample) -> f64 {
        let drivers = self.weather.msis_drivers(s.epoch);
        let mjd = s.epoch.mjd_utc();
        let year = s.epoch.to_gregorian(TimeScale::UTC).0;
        let day_of_year = (mjd.floor() as i64 - civil_to_mjd(year.into(), 1, 1) + 1) as i32;
        let (lat, lon, h) = s.geodetic;
        let input = Input::consistent(
            day_of_year,
            (mjd - mjd.floor()) * 86_400.0,
            h,
            lat.to_degrees(),
            lon.to_degrees(),
            drivers.f107_average,
            drivers.f107,
            MagneticActivity::History { ap: drivers.ap },
        );
        self.model.gtd7d(&input).total_mass
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nrlmsise00::{Switches, Units};

    /// Reference test cases (Brodowski C distribution, DOCUMENTATION): ρ in g/cm³, `gtd7`.
    #[test]
    fn reference_cases() {
        let model = Model::with_switches(Switches {
            units: Units::Native,
            ..Switches::default()
        });
        let input = |alt: f64| Input {
            day_of_year: 172,
            seconds_of_day: 29_000.0,
            altitude_km: alt,
            latitude_deg: 60.0,
            longitude_deg: -70.0,
            local_solar_time_h: 16.0,
            f107_average: 150.0,
            f107: 150.0,
            magnetic: MagneticActivity::Daily { ap: 4.0 },
        };
        for (alt, rho) in [(400.0, 4.075e-15), (1000.0, 2.757e-18), (100.0, 3.584e-10)] {
            let got = model.gtd7(&input(alt)).total_mass;
            assert!((got / rho - 1.0).abs() < 2e-4, "{alt} km: {got:e}");
        }
    }

    #[test]
    fn atmosphere_bounds() {
        use crate::time::Epoch;
        use glam::DVec3;
        let msis = Nrlmsise00::new(Arc::new(SpaceWeather::constant(150.0, 150.0, 4.0)));
        let rho = |h: f64| {
            msis.density(&AtmosphereSample {
                epoch: Epoch::from_utc(2024, 6, 20, 12, 0, 0.0),
                position_inertial_km: DVec3::X,
                position_body_fixed_km: DVec3::X,
                geodetic: (0.3, 1.0, h),
                sun_km: None,
            })
        };
        // kg/m³; monotone, within an order of magnitude of the static table.
        let (a, b) = (rho(300.0), rho(500.0));
        assert!(a > b && b > 0.0);
        for (h, x) in [(300.0, a), (500.0, b)] {
            let r = x / super::super::ExponentialAtmosphere::density_at_height(h);
            assert!((0.1..10.0).contains(&r), "{h} km: ×{r}");
        }
    }
}
