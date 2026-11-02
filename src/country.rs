//! Country and federation normalization and presentation helpers.
//!
//! PGN `WhiteCountry`/`BlackCountry`/`WhiteFed`/`BlackFed` tags and imported
//! metadata are free-form strings. Values are matched against known FIDE, IOC,
//! and ISO 3166-1 country codes and names without guessing from a player's
//! name or language. Placeholders and unknown codes yield `None`. Raw tags
//! in games and databases are preserved untouched.

/// A recognized country or chess federation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Country {
    /// Canonical 3-letter FIDE / IOC country code (e.g. `"NOR"`).
    pub code: &'static str,
    /// 2-letter ISO 3166-1 alpha-2 code (e.g. `"NO"`).
    pub alpha2: &'static str,
    /// English display name (e.g. `"Norway"`).
    pub name: &'static str,
    /// Unicode flag emoji (e.g. `"🇳🇴"`).
    pub flag: &'static str,
    /// Formatted flag-and-code badge for secondary player metadata (e.g. `"🇳🇴 NOR"`).
    pub badge: &'static str,
}

impl Country {
    pub const fn new(
        code: &'static str,
        alpha2: &'static str,
        name: &'static str,
        flag: &'static str,
        badge: &'static str,
    ) -> Self {
        Self {
            code,
            alpha2,
            name,
            flag,
            badge,
        }
    }

    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn alpha2(&self) -> &'static str {
        self.alpha2
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn flag(&self) -> &'static str {
        self.flag
    }

    pub fn badge(&self) -> &'static str {
        self.badge
    }
}

/// Discard known placeholder strings that indicate missing or unknown country metadata.
pub fn is_placeholder(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() {
        return true;
    }
    matches!(s, "?" | "??" | "???" | "-" | "--" | "---" | "0" | "*")
        || s.eq_ignore_ascii_case("none")
        || s.eq_ignore_ascii_case("null")
        || s.eq_ignore_ascii_case("nil")
        || s.eq_ignore_ascii_case("n/a")
        || s.eq_ignore_ascii_case("na")
        || s.eq_ignore_ascii_case("unk")
        || s.eq_ignore_ascii_case("unknown")
}

/// Normalize an optional header tag (such as `WhiteCountry`, `BlackCountry`,
/// `WhiteFed`, or `BlackFed`). Returns `None` if absent, empty, a placeholder,
/// or not a recognized country.
pub fn from_header(raw: Option<&str>) -> Option<Country> {
    normalize(raw?)
}

/// Normalize a raw country string to a canonical [`Country`].
///
/// Accepts 3-letter FIDE/IOC codes (e.g. `"NOR"`), 2-letter ISO codes
/// (e.g. `"NO"`), common ISO-3/name aliases (e.g. `"DEU"`, `"Deutschland"`),
/// and full English names (e.g. `"Norway"`).
///
/// Returns `None` if the input is a placeholder, unknown, or not a recognized
/// country. Never infers or guesses a country from player names.
pub fn normalize(raw: &str) -> Option<Country> {
    let trimmed = raw.trim();
    if is_placeholder(trimmed) {
        return None;
    }

    // Check alias list first (maps alternative codes/names to canonical 3-letter codes)
    if let Some(&(_, target_code)) = ALIASES
        .iter()
        .find(|(alias, _)| alias.eq_ignore_ascii_case(trimmed))
        && let Some(c) = find_by_code(target_code)
    {
        return Some(c);
    }

    // Match by 3-letter code
    if let Some(c) = find_by_code(trimmed) {
        return Some(c);
    }

    // Match by 2-letter ISO alpha-2
    if trimmed.len() == 2
        && let Some(c) = COUNTRIES
            .iter()
            .find(|c| c.alpha2.eq_ignore_ascii_case(trimmed))
            .copied()
    {
        return Some(c);
    }

    // Match by full English name
    if let Some(c) = COUNTRIES
        .iter()
        .find(|c| c.name.eq_ignore_ascii_case(trimmed))
        .copied()
    {
        return Some(c);
    }

    None
}

/// Normalize a raw country string to its canonical 3-letter code (e.g. `"NOR"`).
pub fn normalize_code(raw: &str) -> Option<&'static str> {
    normalize(raw).map(|c| c.code)
}

fn find_by_code(code: &str) -> Option<Country> {
    COUNTRIES
        .iter()
        .find(|c| c.code.eq_ignore_ascii_case(code))
        .copied()
}

static COUNTRIES: &[Country] = &[
    Country::new("AFG", "AF", "Afghanistan", "🇦🇫", "🇦🇫 AFG"),
    Country::new("ALB", "AL", "Albania", "🇦🇱", "🇦🇱 ALB"),
    Country::new("ALG", "DZ", "Algeria", "🇩🇿", "🇩🇿 ALG"),
    Country::new("AND", "AD", "Andorra", "🇦🇩", "🇦🇩 AND"),
    Country::new("ANG", "AO", "Angola", "🇦🇴", "🇦🇴 ANG"),
    Country::new("ARG", "AR", "Argentina", "🇦🇷", "🇦🇷 ARG"),
    Country::new("ARM", "AM", "Armenia", "🇦🇲", "🇦🇲 ARM"),
    Country::new("ARU", "AW", "Aruba", "🇦🇼", "🇦🇼 ARU"),
    Country::new("AUS", "AU", "Australia", "🇦🇺", "🇦🇺 AUS"),
    Country::new("AUT", "AT", "Austria", "🇦🇹", "🇦🇹 AUT"),
    Country::new("AZE", "AZ", "Azerbaijan", "🇦🇿", "🇦🇿 AZE"),
    Country::new("BAH", "BS", "Bahamas", "🇧🇸", "🇧🇸 BAH"),
    Country::new("BRN", "BH", "Bahrain", "🇧🇭", "🇧🇭 BRN"),
    Country::new("BAN", "BD", "Bangladesh", "🇧🇩", "🇧🇩 BAN"),
    Country::new("BAR", "BB", "Barbados", "🇧🇧", "🇧🇧 BAR"),
    Country::new("BLR", "BY", "Belarus", "🇧🇾", "🇧🇾 BLR"),
    Country::new("BEL", "BE", "Belgium", "🇧🇪", "🇧🇪 BEL"),
    Country::new("BER", "BM", "Bermuda", "🇧🇲", "🇧🇲 BER"),
    Country::new("BHU", "BT", "Bhutan", "🇧🇹", "🇧🇹 BHU"),
    Country::new("BOL", "BO", "Bolivia", "🇧🇴", "🇧🇴 BOL"),
    Country::new("BIH", "BA", "Bosnia and Herzegovina", "🇧🇦", "🇧🇦 BIH"),
    Country::new("BOT", "BW", "Botswana", "🇧🇼", "🇧🇼 BOT"),
    Country::new("BRA", "BR", "Brazil", "🇧🇷", "🇧🇷 BRA"),
    Country::new("IVB", "VG", "British Virgin Islands", "🇻🇬", "🇻🇬 IVB"),
    Country::new("BRU", "BN", "Brunei", "🇧🇳", "🇧🇳 BRU"),
    Country::new("BUL", "BG", "Bulgaria", "🇧🇬", "🇧🇬 BUL"),
    Country::new("BUR", "BF", "Burkina Faso", "🇧🇫", "🇧🇫 BUR"),
    Country::new("BDI", "BI", "Burundi", "🇧🇮", "🇧🇮 BDI"),
    Country::new("CAM", "KH", "Cambodia", "🇰🇭", "🇰🇭 CAM"),
    Country::new("CMR", "CM", "Cameroon", "🇨🇲", "🇨🇲 CMR"),
    Country::new("CAN", "CA", "Canada", "🇨🇦", "🇨🇦 CAN"),
    Country::new("CPV", "CV", "Cape Verde", "🇨🇻", "🇨🇻 CPV"),
    Country::new("CAY", "KY", "Cayman Islands", "🇰🇾", "🇰🇾 CAY"),
    Country::new("CAF", "CF", "Central African Republic", "🇨🇫", "🇨🇫 CAF"),
    Country::new("CHA", "TD", "Chad", "🇹🇩", "🇹🇩 CHA"),
    Country::new("CHI", "CL", "Chile", "🇨🇱", "🇨🇱 CHI"),
    Country::new("CHN", "CN", "China", "🇨🇳", "🇨🇳 CHN"),
    Country::new("TPE", "TW", "Chinese Taipei", "🇹🇼", "🇹🇼 TPE"),
    Country::new("COL", "CO", "Colombia", "🇨🇴", "🇨🇴 COL"),
    Country::new("COM", "KM", "Comoros", "🇰🇲", "🇰🇲 COM"),
    Country::new("CGO", "CG", "Congo", "🇨🇬", "🇨🇬 CGO"),
    Country::new("CRC", "CR", "Costa Rica", "🇨🇷", "🇨🇷 CRC"),
    Country::new("CIV", "CI", "Cote d'Ivoire", "🇨🇮", "🇨🇮 CIV"),
    Country::new("CRO", "HR", "Croatia", "🇭🇷", "🇭🇷 CRO"),
    Country::new("CUB", "CU", "Cuba", "🇨🇺", "🇨🇺 CUB"),
    Country::new("CYP", "CY", "Cyprus", "🇨🇾", "🇨🇾 CYP"),
    Country::new("CZE", "CZ", "Czech Republic", "🇨🇿", "🇨🇿 CZE"),
    Country::new("DEN", "DK", "Denmark", "🇩🇰", "🇩🇰 DEN"),
    Country::new("DJI", "DJ", "Djibouti", "🇩🇯", "🇩🇯 DJI"),
    Country::new("DOM", "DO", "Dominican Republic", "🇩🇴", "🇩🇴 DOM"),
    Country::new("ECU", "EC", "Ecuador", "🇪🇨", "🇪🇨 ECU"),
    Country::new("EGY", "EG", "Egypt", "🇪🇬", "🇪🇬 EGY"),
    Country::new("ESA", "SV", "El Salvador", "🇸🇻", "🇸🇻 ESA"),
    Country::new("GBR", "GB", "United Kingdom", "🇬🇧", "🇬🇧 GBR"),
    Country::new("ENG", "GB", "England", "🏴󠁧󠁢󠁥󠁮󠁧󠁿", "🏴󠁧󠁢󠁥󠁮󠁧󠁿 ENG"),
    Country::new("SCO", "GB", "Scotland", "🏴󠁧󠁢󠁳󠁣󠁴󠁿", "🏴󠁧󠁢󠁳󠁣󠁴󠁿 SCO"),
    Country::new("WLS", "GB", "Wales", "🏴󠁧󠁢󠁷󠁬󠁳󠁿", "🏴󠁧󠁢󠁷󠁬󠁳󠁿 WLS"),
    Country::new("GEQ", "GQ", "Equatorial Guinea", "🇬🇶", "🇬🇶 GEQ"),
    Country::new("ERI", "ER", "Eritrea", "🇪🇷", "🇪🇷 ERI"),
    Country::new("EST", "EE", "Estonia", "🇪🇪", "🇪🇪 EST"),
    Country::new("SWZ", "SZ", "Eswatini", "🇸🇿", "🇸🇿 SWZ"),
    Country::new("ETH", "ET", "Ethiopia", "🇪🇹", "🇪🇹 ETH"),
    Country::new("FAI", "FO", "Faroe Islands", "🇫🇴", "🇫🇴 FAI"),
    Country::new("FIJ", "FJ", "Fiji", "🇫🇯", "🇫🇯 FIJ"),
    Country::new("FIN", "FI", "Finland", "🇫🇮", "🇫🇮 FIN"),
    Country::new("FRA", "FR", "France", "🇫🇷", "🇫🇷 FRA"),
    Country::new("GAB", "GA", "Gabon", "🇬🇦", "🇬🇦 GAB"),
    Country::new("GAM", "GM", "Gambia", "🇬🇲", "🇬🇲 GAM"),
    Country::new("GEO", "GE", "Georgia", "🇬🇪", "🇬🇪 GEO"),
    Country::new("GER", "DE", "Germany", "🇩🇪", "🇩🇪 GER"),
    Country::new("GHA", "GH", "Ghana", "🇬🇭", "🇬🇭 GHA"),
    Country::new("GRE", "GR", "Greece", "🇬🇷", "🇬🇷 GRE"),
    Country::new("GUA", "GT", "Guatemala", "🇬🇹", "🇬🇹 GUA"),
    Country::new("GCI", "GG", "Guernsey", "🇬🇬", "🇬🇬 GCI"),
    Country::new("GUY", "GY", "Guyana", "🇬🇾", "🇬🇾 GUY"),
    Country::new("HAI", "HT", "Haiti", "🇭🇹", "🇭🇹 HAI"),
    Country::new("HON", "HN", "Honduras", "🇭🇳", "🇭🇳 HON"),
    Country::new("HKG", "HK", "Hong Kong", "🇭🇰", "🇭🇰 HKG"),
    Country::new("HUN", "HU", "Hungary", "🇭🇺", "🇭🇺 HUN"),
    Country::new("ISL", "IS", "Iceland", "🇮🇸", "🇮🇸 ISL"),
    Country::new("IND", "IN", "India", "🇮🇳", "🇮🇳 IND"),
    Country::new("INA", "ID", "Indonesia", "🇮🇩", "🇮🇩 INA"),
    Country::new("IRI", "IR", "Iran", "🇮🇷", "🇮🇷 IRI"),
    Country::new("IRQ", "IQ", "Iraq", "🇮🇶", "🇮🇶 IRQ"),
    Country::new("IRL", "IE", "Ireland", "🇮🇪", "🇮🇪 IRL"),
    Country::new("IMN", "IM", "Isle of Man", "🇮🇲", "🇮🇲 IMN"),
    Country::new("ISR", "IL", "Israel", "🇮🇱", "🇮🇱 ISR"),
    Country::new("ITA", "IT", "Italy", "🇮🇹", "🇮🇹 ITA"),
    Country::new("JAM", "JM", "Jamaica", "🇯🇲", "🇯🇲 JAM"),
    Country::new("JPN", "JP", "Japan", "🇯🇵", "🇯🇵 JPN"),
    Country::new("JCI", "JE", "Jersey", "🇯🇪", "🇯🇪 JCI"),
    Country::new("JOR", "JO", "Jordan", "🇯🇴", "🇯🇴 JOR"),
    Country::new("KAZ", "KZ", "Kazakhstan", "🇰🇿", "🇰🇿 KAZ"),
    Country::new("KEN", "KE", "Kenya", "🇰🇪", "🇰🇪 KEN"),
    Country::new("KOS", "XK", "Kosovo", "🇽🇰", "🇽🇰 KOS"),
    Country::new("KUW", "KW", "Kuwait", "🇰🇼", "🇰🇼 KUW"),
    Country::new("KGZ", "KG", "Kyrgyzstan", "🇰🇬", "🇰🇬 KGZ"),
    Country::new("LAO", "LA", "Laos", "🇱🇦", "🇱🇦 LAO"),
    Country::new("LAT", "LV", "Latvia", "🇱🇻", "🇱🇻 LAT"),
    Country::new("LBN", "LB", "Lebanon", "🇱🇧", "🇱🇧 LBN"),
    Country::new("LES", "LS", "Lesotho", "🇱🇸", "🇱🇸 LES"),
    Country::new("LBR", "LR", "Liberia", "🇱🇷", "🇱🇷 LBR"),
    Country::new("LBA", "LY", "Libya", "🇱🇾", "🇱🇾 LBA"),
    Country::new("LIE", "LI", "Liechtenstein", "🇱🇮", "🇱🇮 LIE"),
    Country::new("LTU", "LT", "Lithuania", "🇱🇹", "🇱🇹 LTU"),
    Country::new("LUX", "LU", "Luxembourg", "🇱🇺", "🇱🇺 LUX"),
    Country::new("MAC", "MO", "Macau", "🇲🇴", "🇲🇴 MAC"),
    Country::new("MAD", "MG", "Madagascar", "🇲🇬", "🇲🇬 MAD"),
    Country::new("MAW", "MW", "Malawi", "🇲🇼", "🇲🇼 MAW"),
    Country::new("MAS", "MY", "Malaysia", "🇲🇾", "🇲🇾 MAS"),
    Country::new("MDV", "MV", "Maldives", "🇲🇻", "🇲🇻 MDV"),
    Country::new("MLI", "ML", "Mali", "🇲🇱", "🇲🇱 MLI"),
    Country::new("MLT", "MT", "Malta", "🇲🇹", "🇲🇹 MLT"),
    Country::new("MTN", "MR", "Mauritania", "🇲🇷", "🇲🇷 MTN"),
    Country::new("MRI", "MU", "Mauritius", "🇲🇺", "🇲🇺 MRI"),
    Country::new("MEX", "MX", "Mexico", "🇲🇽", "🇲🇽 MEX"),
    Country::new("MDA", "MD", "Moldova", "🇲🇩", "🇲🇩 MDA"),
    Country::new("MNC", "MC", "Monaco", "🇲🇨", "🇲🇨 MNC"),
    Country::new("MGL", "MN", "Mongolia", "🇲🇳", "🇲🇳 MGL"),
    Country::new("MNE", "ME", "Montenegro", "🇲🇪", "🇲🇪 MNE"),
    Country::new("MAR", "MA", "Morocco", "🇲🇦", "🇲🇦 MAR"),
    Country::new("MOZ", "MZ", "Mozambique", "🇲🇿", "🇲🇿 MOZ"),
    Country::new("MYA", "MM", "Myanmar", "🇲🇲", "🇲🇲 MYA"),
    Country::new("NAM", "NA", "Namibia", "🇳🇦", "🇳🇦 NAM"),
    Country::new("NRU", "NR", "Nauru", "🇳🇷", "🇳🇷 NRU"),
    Country::new("NEP", "NP", "Nepal", "🇳🇵", "🇳🇵 NEP"),
    Country::new("NED", "NL", "Netherlands", "🇳🇱", "🇳🇱 NED"),
    Country::new("NZL", "NZ", "New Zealand", "🇳🇿", "🇳🇿 NZL"),
    Country::new("NCA", "NI", "Nicaragua", "🇳🇮", "🇳🇮 NCA"),
    Country::new("NIG", "NE", "Niger", "🇳🇪", "🇳🇪 NIG"),
    Country::new("NGR", "NG", "Nigeria", "🇳🇬", "🇳🇬 NGR"),
    Country::new("MKD", "MK", "North Macedonia", "🇲🇰", "🇲🇰 MKD"),
    Country::new("NOR", "NO", "Norway", "🇳🇴", "🇳🇴 NOR"),
    Country::new("OMA", "OM", "Oman", "🇴🇲", "🇴🇲 OMA"),
    Country::new("PAK", "PK", "Pakistan", "🇵🇰", "🇵🇰 PAK"),
    Country::new("PLW", "PW", "Palau", "🇵🇼", "🇵🇼 PLW"),
    Country::new("PLE", "PS", "Palestine", "🇵🇸", "🇵🇸 PLE"),
    Country::new("PAN", "PA", "Panama", "🇵🇦", "🇵🇦 PAN"),
    Country::new("PNG", "PG", "Papua New Guinea", "🇵🇬", "🇵🇬 PNG"),
    Country::new("PAR", "PY", "Paraguay", "🇵🇾", "🇵🇾 PAR"),
    Country::new("PER", "PE", "Peru", "🇵🇪", "🇵🇪 PER"),
    Country::new("PHI", "PH", "Philippines", "🇵🇭", "🇵🇭 PHI"),
    Country::new("POL", "PL", "Poland", "🇵🇱", "🇵🇱 POL"),
    Country::new("POR", "PT", "Portugal", "🇵🇹", "🇵🇹 POR"),
    Country::new("PUR", "PR", "Puerto Rico", "🇵🇷", "🇵🇷 PUR"),
    Country::new("QAT", "QA", "Qatar", "🇶🇦", "🇶🇦 QAT"),
    Country::new("ROU", "RO", "Romania", "🇷🇴", "🇷🇴 ROU"),
    Country::new("RUS", "RU", "Russia", "🇷🇺", "🇷🇺 RUS"),
    Country::new("RWA", "RW", "Rwanda", "🇷🇼", "🇷🇼 RWA"),
    Country::new("SMR", "SM", "San Marino", "🇸🇲", "🇸🇲 SMR"),
    Country::new("STP", "ST", "Sao Tome and Principe", "🇸🇹", "🇸🇹 STP"),
    Country::new("KSA", "SA", "Saudi Arabia", "🇸🇦", "🇸🇦 KSA"),
    Country::new("SEN", "SN", "Senegal", "🇸🇳", "🇸🇳 SEN"),
    Country::new("SRB", "RS", "Serbia", "🇷🇸", "🇷🇸 SRB"),
    Country::new("SEY", "SC", "Seychelles", "🇸🇨", "🇸🇨 SEY"),
    Country::new("SLE", "SL", "Sierra Leone", "🇸🇱", "🇸🇱 SLE"),
    Country::new("SGP", "SG", "Singapore", "🇸🇬", "🇸🇬 SGP"),
    Country::new("SVK", "SK", "Slovakia", "🇸🇰", "🇸🇰 SVK"),
    Country::new("SLO", "SI", "Slovenia", "🇸🇮", "🇸🇮 SLO"),
    Country::new("SOL", "SB", "Solomon Islands", "🇸🇧", "🇸🇧 SOL"),
    Country::new("SOM", "SO", "Somalia", "🇸🇴", "🇸🇴 SOM"),
    Country::new("RSA", "ZA", "South Africa", "🇿🇦", "🇿🇦 RSA"),
    Country::new("KOR", "KR", "South Korea", "🇰🇷", "🇰🇷 KOR"),
    Country::new("SSD", "SS", "South Sudan", "🇸🇸", "🇸🇸 SSD"),
    Country::new("ESP", "ES", "Spain", "🇪🇸", "🇪🇸 ESP"),
    Country::new("SRI", "LK", "Sri Lanka", "🇱🇰", "🇱🇰 SRI"),
    Country::new("SUD", "SD", "Sudan", "🇸🇩", "🇸🇩 SUD"),
    Country::new("SUR", "SR", "Suriname", "🇸🇷", "🇸🇷 SUR"),
    Country::new("SWE", "SE", "Sweden", "🇸🇪", "🇸🇪 SWE"),
    Country::new("SUI", "CH", "Switzerland", "🇨🇭", "🇨🇭 SUI"),
    Country::new("SYR", "SY", "Syria", "🇸🇾", "🇸🇾 SYR"),
    Country::new("TJK", "TJ", "Tajikistan", "🇹🇯", "🇹🇯 TJK"),
    Country::new("TAN", "TZ", "Tanzania", "🇹🇿", "🇹🇿 TAN"),
    Country::new("THA", "TH", "Thailand", "🇹🇭", "🇹🇭 THA"),
    Country::new("TLS", "TL", "Timor-Leste", "🇹🇱", "🇹🇱 TLS"),
    Country::new("TOG", "TG", "Togo", "🇹🇬", "🇹🇬 TOG"),
    Country::new("TGA", "TO", "Tonga", "🇹🇴", "🇹🇴 TGA"),
    Country::new("TTO", "TT", "Trinidad and Tobago", "🇹🇹", "🇹🇹 TTO"),
    Country::new("TUN", "TN", "Tunisia", "🇹🇳", "🇹🇳 TUN"),
    Country::new("TUR", "TR", "Turkey", "🇹🇷", "🇹🇷 TUR"),
    Country::new("TKM", "TM", "Turkmenistan", "🇹🇲", "🇹🇲 TKM"),
    Country::new("UGA", "UG", "Uganda", "🇺🇬", "🇺🇬 UGA"),
    Country::new("UKR", "UA", "Ukraine", "🇺🇦", "🇺🇦 UKR"),
    Country::new("UAE", "AE", "United Arab Emirates", "🇦🇪", "🇦🇪 UAE"),
    Country::new("USA", "US", "United States", "🇺🇸", "🇺🇸 USA"),
    Country::new("URU", "UY", "Uruguay", "🇺🇾", "🇺🇾 URU"),
    Country::new("ISV", "VI", "US Virgin Islands", "🇻🇮", "🇻🇮 ISV"),
    Country::new("UZB", "UZ", "Uzbekistan", "🇺🇿", "🇺🇿 UZB"),
    Country::new("VAN", "VU", "Vanuatu", "🇻🇺", "🇻🇺 VAN"),
    Country::new("VEN", "VE", "Venezuela", "🇻🇪", "🇻🇪 VEN"),
    Country::new("VIE", "VN", "Vietnam", "🇻🇳", "🇻🇳 VIE"),
    Country::new("YEM", "YE", "Yemen", "🇾🇪", "🇾🇪 YEM"),
    Country::new("ZAM", "ZM", "Zambia", "🇿🇲", "🇿🇲 ZAM"),
    Country::new("ZIM", "ZW", "Zimbabwe", "🇿🇼", "🇿🇼 ZIM"),
    Country::new("URS", "SU", "Soviet Union", "🇸🇺", "🇸🇺 URS"),
    Country::new("YUG", "YU", "Yugoslavia", "🇾🇺", "🇾🇺 YUG"),
    Country::new("GDR", "DD", "East Germany", "🇩🇩", "🇩🇩 GDR"),
    Country::new("FRG", "DE", "West Germany", "🇩🇪", "🇩🇪 FRG"),
    Country::new("TCH", "CS", "Czechoslovakia", "🇨🇸", "🇨🇸 TCH"),
    Country::new("SCG", "CS", "Serbia and Montenegro", "🇷🇸", "🇷🇸 SCG"),
    Country::new("FID", "UN", "FIDE", "🌐", "🌐 FID"),
];

static ALIASES: &[(&str, &str)] = &[
    ("DEU", "GER"),
    ("NLD", "NED"),
    ("HRV", "CRO"),
    ("GRC", "GRE"),
    ("CHE", "SUI"),
    ("ZAF", "RSA"),
    ("LVA", "LAT"),
    ("DNK", "DEN"),
    ("PRT", "POR"),
    ("SVN", "SLO"),
    ("MNG", "MGL"),
    ("BGR", "BUL"),
    ("ROM", "ROU"),
    ("CHL", "CHI"),
    ("SIN", "SGP"),
    ("PHL", "PHI"),
    ("IDN", "INA"),
    ("FRO", "FAI"),
    ("AGO", "ANG"),
    ("BRB", "BAR"),
    ("BHR", "BRN"),
    ("BGD", "BAN"),
    ("BWA", "BOT"),
    ("KHM", "CAM"),
    ("MYS", "MAS"),
    ("NER", "NIG"),
    ("NGA", "NGR"),
    ("URY", "URU"),
    ("VNM", "VIE"),
    ("ZMB", "ZAM"),
    ("ZWE", "ZIM"),
    ("UK", "GBR"),
    ("Great Britain", "GBR"),
    ("Britain", "GBR"),
    ("Russia", "RUS"),
    ("Russian Federation", "RUS"),
    ("Rossiya", "RUS"),
    ("USA", "USA"),
    ("United States of America", "USA"),
    ("U.S.A.", "USA"),
    ("U.S.", "USA"),
    ("US", "USA"),
    ("Czechia", "CZE"),
    ("UAE", "UAE"),
    ("South Korea", "KOR"),
    ("Korea, South", "KOR"),
    ("Republic of Korea", "KOR"),
    ("Moldova, Republic of", "MDA"),
    ("USSR", "URS"),
    ("CCCP", "URS"),
    ("DDR", "GDR"),
    ("BRD", "FRG"),
    ("Deutschland", "GER"),
    ("España", "ESP"),
    ("Italia", "ITA"),
    ("Norge", "NOR"),
    ("Sverige", "SWE"),
    ("Polska", "POL"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_standard_fide_codes() {
        let nor = normalize("NOR").expect("NOR");
        assert_eq!(nor.code(), "NOR");
        assert_eq!(nor.alpha2(), "NO");
        assert_eq!(nor.name(), "Norway");
        assert_eq!(nor.flag(), "🇳🇴");
        assert_eq!(nor.badge(), "🇳🇴 NOR");

        let usa = normalize("USA").expect("USA");
        assert_eq!(usa.code(), "USA");
        assert_eq!(usa.alpha2(), "US");
        assert_eq!(usa.flag(), "🇺🇸");
        assert_eq!(usa.badge(), "🇺🇸 USA");

        let ger = normalize("GER").expect("GER");
        assert_eq!(ger.code(), "GER");
        assert_eq!(ger.alpha2(), "DE");
        assert_eq!(ger.flag(), "🇩🇪");

        let ind = normalize("IND").expect("IND");
        assert_eq!(ind.code(), "IND");
        assert_eq!(ind.alpha2(), "IN");
        assert_eq!(ind.flag(), "🇮🇳");

        let chn = normalize("CHN").expect("CHN");
        assert_eq!(chn.code(), "CHN");
        assert_eq!(chn.alpha2(), "CN");
        assert_eq!(chn.flag(), "🇨🇳");

        let eng = normalize("ENG").expect("ENG");
        assert_eq!(eng.code(), "ENG");
        assert_eq!(eng.alpha2(), "GB");
        assert_eq!(eng.flag(), "🏴󠁧󠁢󠁥󠁮󠁧󠁿");
        assert_eq!(eng.badge(), "🏴󠁧󠁢󠁥󠁮󠁧󠁿 ENG");
    }

    #[test]
    fn normalizes_iso_alpha2_codes() {
        assert_eq!(normalize("NO").map(|c| c.code), Some("NOR"));
        assert_eq!(normalize("no").map(|c| c.code), Some("NOR"));
        assert_eq!(normalize("US").map(|c| c.code), Some("USA"));
        assert_eq!(normalize("us").map(|c| c.code), Some("USA"));
        assert_eq!(normalize("DE").map(|c| c.code), Some("GER"));
        assert_eq!(normalize("de").map(|c| c.code), Some("GER"));
        assert_eq!(normalize("FR").map(|c| c.code), Some("FRA"));
        assert_eq!(normalize("IN").map(|c| c.code), Some("IND"));
        assert_eq!(normalize("CN").map(|c| c.code), Some("CHN"));
    }

    #[test]
    fn normalizes_case_and_whitespace() {
        assert_eq!(normalize(" nor ").map(|c| c.code), Some("NOR"));
        assert_eq!(normalize("Nor").map(|c| c.code), Some("NOR"));
        assert_eq!(normalize("  uSa  ").map(|c| c.code), Some("USA"));
        assert_eq!(
            normalize(
                "	gEr
"
            )
            .map(|c| c.code),
            Some("GER")
        );
    }

    #[test]
    fn normalizes_full_names() {
        assert_eq!(normalize("Norway").map(|c| c.code), Some("NOR"));
        assert_eq!(normalize("norway").map(|c| c.code), Some("NOR"));
        assert_eq!(normalize("United States").map(|c| c.code), Some("USA"));
        assert_eq!(normalize("Germany").map(|c| c.code), Some("GER"));
        assert_eq!(normalize("France").map(|c| c.code), Some("FRA"));
        assert_eq!(normalize("India").map(|c| c.code), Some("IND"));
        assert_eq!(normalize("China").map(|c| c.code), Some("CHN"));
    }

    #[test]
    fn normalizes_iso3_and_aliases() {
        assert_eq!(normalize("DEU").map(|c| c.code), Some("GER"));
        assert_eq!(normalize("deu").map(|c| c.code), Some("GER"));
        assert_eq!(normalize("NLD").map(|c| c.code), Some("NED"));
        assert_eq!(normalize("HRV").map(|c| c.code), Some("CRO"));
        assert_eq!(normalize("CHE").map(|c| c.code), Some("SUI"));
        assert_eq!(normalize("Russia").map(|c| c.code), Some("RUS"));
        assert_eq!(normalize("Deutschland").map(|c| c.code), Some("GER"));
        assert_eq!(normalize("España").map(|c| c.code), Some("ESP"));
        assert_eq!(normalize("Norge").map(|c| c.code), Some("NOR"));
        assert_eq!(normalize("UK").map(|c| c.code), Some("GBR"));
        assert_eq!(normalize("Great Britain").map(|c| c.code), Some("GBR"));
    }

    #[test]
    fn normalizes_historical_federations() {
        assert_eq!(normalize("URS").map(|c| c.code), Some("URS"));
        assert_eq!(normalize("USSR").map(|c| c.code), Some("URS"));
        assert_eq!(normalize("Soviet Union").map(|c| c.code), Some("URS"));
        assert_eq!(normalize("YUG").map(|c| c.code), Some("YUG"));
        assert_eq!(normalize("GDR").map(|c| c.code), Some("GDR"));
        assert_eq!(normalize("DDR").map(|c| c.code), Some("GDR"));
        assert_eq!(normalize("FRG").map(|c| c.code), Some("FRG"));
        assert_eq!(normalize("TCH").map(|c| c.code), Some("TCH"));
        assert_eq!(normalize("FID").map(|c| c.code), Some("FID"));
    }

    #[test]
    fn rejects_unknown_codes() {
        for unknown in ["XYZ", "ABC", "ZZ", "123", "NONEXISTENT", "FOO", "BAR1"] {
            assert_eq!(normalize(unknown), None, "unknown code {unknown:?}");
            assert_eq!(normalize_code(unknown), None, "unknown code {unknown:?}");
        }
    }

    #[test]
    fn rejects_placeholders_and_empty() {
        assert_eq!(from_header(None), None);
        for placeholder in [
            "", "   ", "	
", "?", "??", "???", "-", "--", "---", "0", "*", "None", "none", "null",
            "nil", "N/A", "n/a", "NA", "UNK", "unk", "Unknown", "unknown",
        ] {
            assert_eq!(normalize(placeholder), None, "placeholder {placeholder:?}");
            assert_eq!(from_header(Some(placeholder)), None);
        }
    }

    #[test]
    fn never_guesses_from_player_name() {
        for name in [
            "Garry Kasparov",
            "Magnus Carlsen",
            "Bobby Fischer",
            "Viswanathan Anand",
            "Ding Liren",
        ] {
            assert_eq!(normalize(name), None, "player name {name:?}");
        }
    }

    #[test]
    fn country_badge_format() {
        let c = normalize("NOR").unwrap();
        assert_eq!(c.badge, "🇳🇴 NOR");
        let ger = normalize("GER").unwrap();
        assert_eq!(ger.badge, "🇩🇪 GER");
    }
}
