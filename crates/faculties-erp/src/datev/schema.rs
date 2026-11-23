//! Stable identities and encodings for accounting batches and booking rows.

use triblespace::core::id::{id_hex, Id};
use triblespace::core::inline::encodings::money::Euro;
use triblespace::macros::attributes;
use triblespace::prelude::*;

pub const KIND_BOOKING: Id = id_hex!("C25D0D1ACF8D046C93AE57EE1409B011");

pub const KIND_ACCOUNT: Id = id_hex!("70D5CA6045A73076E616A7A4C483A6A6");

pub const KIND_BATCH: Id = id_hex!("14B5E9140513987F81E08DEB202C7049");

pub mod booking {
    use super::*;

    attributes! {
        /// Signed amount derived from the magnitude and Soll/Haben indicator.
        "0AA1DE7B251B3BB906DD5C09B635F082" as pub amount: inlineencodings::Currency<Euro>;
        /// Account to which the booking is posted.
        "E242F2A33C1405D69EED6E91CC63E4EA" as pub account: inlineencodings::GenId;
        /// Counter-entry account.
        "631499706921571582F02B8B2688C72B" as pub contra_account: inlineencodings::GenId;
        /// Document date derived from Belegdatum and the fiscal year.
        "5513FAF20A4CDCE7B34915939BCFCD44" as pub posted_on: inlineencodings::NsTAIInterval;
        /// Document reference from Belegfeld 1.
        "9A467F8FEA35C053DD51EAC8453BD424" as pub document_number: inlineencodings::ShortString;
        /// Additional reference from Belegfeld 2.
        "B0E30F032019E29CA67D25E1F951A449" as pub document_field_2: inlineencodings::ShortString;
        /// Booking description from Buchungstext.
        "8005CCDA6B05DEC60188600C685738F3" as pub text: inlineencodings::ShortString;
        /// Tax treatment code from BU-Schlüssel.
        "6224880A1FBC9C6E6EB4C14D584DF5B5" as pub tax_key: inlineencodings::ShortString;
        /// Batch that supplied this booking.
        "8D9701357D7F02B4D1DC2C38D6AE276F" as pub batch: inlineencodings::GenId;
        /// Number of equivalent rows in the source batch.
        "02D3ACED7BECD1A5143F55AFDB39DE6F" as pub source_rows: inlineencodings::I256BE;
    }
}

pub mod account {
    use super::*;

    attributes! {
        /// Account number in the source chart of accounts.
        "D9BE6E604CCE384FCE53D93D403ED233" as pub number: inlineencodings::ShortString;
        /// Client whose chart contains the account.
        "1D162855A0A550B19CBA8AC8E8380BCF" as pub client: inlineencodings::I256BE;
    }
}

pub mod batch {
    use super::*;

    attributes! {
        /// Exporting adviser number from the batch header.
        "AD548C94EEE6E887B4B162020FDEE08E" as pub consultant: inlineencodings::I256BE;
        /// Client number from the batch header.
        "F96B28DE6058EEA0C292B2BEBD57D1A8" as pub client: inlineencodings::I256BE;
        /// Reporting period declared by the batch header.
        "530B24C18D8573995D9E661F2CB252DA" as pub period: inlineencodings::NsTAIInterval;
    }
}
