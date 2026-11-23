//! Stable anchors, domain codes and attribute encodings for Revolver records.
//! Each business record has an anchor and independently observed snapshots.

use triblespace::core::inline::encodings::money::{Amount, Euro};
use triblespace::macros::id_hex;
use triblespace::prelude::*;

pub type Euros = Amount<Euro>;

pub const DEFAULT_SCOPE_ID: Id = id_hex!("ED5F458ABF885758CC60D3A4A4B9CD2B");

pub const KIND_PARTY: Id = id_hex!("589D513685000A7F74237150A0C25D09");

pub const KIND_SALES_DOCUMENT: Id = id_hex!("592963F2EBE5D88DFE9FBC3221D6707C");

pub const KIND_SALES_LINE: Id = id_hex!("75753F08680312044CA2924D5CE3D568");

pub const KIND_PAYMENT: Id = id_hex!("AEF53874B3A6B27C57D862E9B636E0A7");

pub const KIND_PAYMENT_ALLOCATION: Id = id_hex!("DBB069FD65392C66C04E80483152870F");

pub const KIND_CORPORATE_GROUP: Id = id_hex!("3D1980B6A4335AAA39180D7A4B7C584F");

pub mod group {
    use super::*;
    attributes! {
        /// Member party anchor. The set of members participates in the group state.
        "1B94A38193C496E82F2D6FA4304DDAD0" as pub member: inlineencodings::GenId;
    }
}

pub const KIND_CODE: Id = id_hex!("DFBE0B27D23ED23C8448B1136DCBCA53");

pub const KIND_CODE_DOMAIN: Id = id_hex!("F6E80E057BB0A36D2367DF243E017795");

pub const KIND_SNAPSHOT: Id = id_hex!("6A05AF5AF0E5F190632D1D8AAF3A1B44");

pub const KIND_PROJECT: Id = id_hex!("95208ECD0E8671E0B9CF7315397749A0");

pub const KIND_PROJECT_STEP: Id = id_hex!("907C8A43DBB4F5BD1DE7BAF325C58E4E");

pub const KIND_PURCHASE_DOCUMENT: Id = id_hex!("77B1ABB230909C697D937F8F8947EEBB");

pub const KIND_PURCHASE_LINE: Id = id_hex!("F17B39C46C67B950AD6B82B1036296F0");

pub const KIND_TIME_ENTRY: Id = id_hex!("B1A88811074E894E7D082FA5B8D55D7A");

pub const KIND_TIME_LINE: Id = id_hex!("FF47E2368EF8942062C298B43DD62939");

pub const KIND_ARTICLE: Id = id_hex!("917358D3A2708301C4F24DD29F791628");

pub const KIND_ARTICLE_SUPPLY: Id = id_hex!("B0AC6208026977C4CF0AFA230DA74082");

pub const KIND_ARTICLE_PRICE: Id = id_hex!("095229C05FD8291B5817A9A4D8D0FA8D");

pub const KIND_EMPLOYEE: Id = id_hex!("6756B0DE450A6C388D495C5A07C703FA");

pub const KIND_OPPORTUNITY: Id = id_hex!("F68C4831CA1B1EB6B14CD00BDB0CCDEA");

pub const KIND_OPPORTUNITY_STEP: Id = id_hex!("D62095AF2739F2E493A02CF291455D83");

pub const KIND_ACTIVITY: Id = id_hex!("E23349A5E366C22BECF70495246729C5");

pub const KIND_HR_REQUEST: Id = id_hex!("29C77D74B840F95C7A22EF99539C4EF4");

pub const KIND_TASK: Id = id_hex!("78171D06975807E9DD71FE8E3087F96F");

pub const KIND_USER: Id = id_hex!("102A147CDC9D46D0D49EED7762C0F71C");

pub const DOMAIN_DOCTYPE: &str = "revolver/doctype";
pub const DOMAIN_ADDRESS_TYPE: &str = "revolver/l_256/address-type";
pub const DOMAIN_CURRENCY: &str = "revolver/currency";
pub const DOMAIN_RECORD_STATE: &str = "revolver/l_261/state";
pub const DOMAIN_POSITION_TYPE: &str = "revolver/l_261_12/position-type";
pub const DOMAIN_SALES_TAX_CLASS: &str = "revolver/l_261_12/tax-class";
pub const DOMAIN_PAYMENT_MODE: &str = "revolver/l_264/payment-mode";
pub const DOMAIN_PROJECT_STATUS: &str = "revolver/l_259/status";
pub const DOMAIN_PROJECT_DIVISION: &str = "revolver/l_259/division";
pub const DOMAIN_STEP_UNIT: &str = "revolver/l_259_10/unit";
pub const DOMAIN_STEP_TAX_CLASS: &str = "revolver/l_259_10/tax-class";
pub const DOMAIN_BILLING_MODE: &str = "revolver/l_259_10/billing-mode";
pub const DOMAIN_DELIVERY_METHOD: &str = "revolver/l_260/delivery-method";
pub const DOMAIN_PURCHASE_DIVISION: &str = "revolver/l_260/division";
pub const DOMAIN_PRICE_SELECTION: &str = "revolver/l_260_12/price-selection";
pub const DOMAIN_PURCHASE_TAX_CLASS: &str = "revolver/l_260_12/tax-class";
pub const DOMAIN_ARTICLE_CATEGORY: &str = "revolver/l_262/category";
pub const DOMAIN_ARTICLE_TAX_CLASS: &str = "revolver/l_262/tax-class";
pub const DOMAIN_PRICE_CATEGORY: &str = "revolver/l_262_23/price-category";
pub const DOMAIN_EMPLOYEE_STATUS: &str = "revolver/l_268/status";
pub const DOMAIN_EMPLOYEE_MARITAL_STATUS: &str = "revolver/l_268/marital-status";
pub const DOMAIN_EMPLOYEE_TAX_CLASS: &str = "revolver/l_268/tax-class";
pub const DOMAIN_OPPORTUNITY_STATUS: &str = "revolver/l_269/status";
pub const DOMAIN_OPPORTUNITY_SOURCE: &str = "revolver/l_269/source";
pub const DOMAIN_NEXT_STEP_TYPE: &str = "revolver/l_269/next-step-type";
pub const DOMAIN_OPPORTUNITY_STEP_CATEGORY: &str = "revolver/l_269_7/category";
pub const DOMAIN_ACTIVITY_CATEGORY: &str = "revolver/l_258/category";
pub const DOMAIN_ACTIVITY_STATUS: &str = "revolver/l_258/appointment-status";
pub const DOMAIN_REQUEST_DOCUMENT_TYPE: &str = "revolver/l_270hjmqctccyqlqckq/document-type";
pub const DOMAIN_TASK_CATEGORY: &str = "revolver/l_220/category";

pub mod revolver {
    use super::*;
    attributes! {
        /// Source tenant number. Part of the identity of a document.
        "DE24D34C8E28FFCE1150302F2129C001" as pub mandant: inlineencodings::I256BE;
        /// Source document number within the tenant and module.
        "3815F7363B7E8848CEFC4A8C98656D6D" as pub record: inlineencodings::I256BE;
        /// Source item number within the parent document.
        "06A7EB488C50CB196A05DD35239762F3" as pub position: inlineencodings::I256BE;
        /// Anchor of the record described by this observation.
        "C12945D7D62A3C56B4D827A0E55A1CEC" as pub of: inlineencodings::GenId;
        /// Intrinsic digest of the observed field values, independent of the observation time.
        "62BC6070694692F3F482CE886C284BBD" as pub content: inlineencodings::GenId;
    }
}

pub mod record {
    use super::*;
    attributes! {
        /// Document type in the Revolver document-type vocabulary.
        "26E46000E31050A6E2A5E4741DB9580B" as pub doctype: inlineencodings::GenId;
        /// Revolver column `credate`.
        "D93B87A29917C47B3253AFE7BA95E048" as pub created: inlineencodings::NsTAIInterval;
        /// Revolver column `moddate`.
        "37417C0EA6ECBE648766401517E08579" as pub modified: inlineencodings::NsTAIInterval;
        /// Revolver column `trashed`.
        "46C762F825F0DE996435604D7D4B221A" as pub trashed: inlineencodings::Boolean;
        /// Revolver column `deactivated`.
        "49CE015D81F0BB90170DA94ABAD1F4CA" as pub deactivated: inlineencodings::Boolean;
        /// Number of physical source rows represented by this record.
        "BD4B4232D3E285F4EA18348FF6D20C2A" as pub source_rows: inlineencodings::I256BE;
        /// Revolver column `state`.
        "074C921C507706E6A547E5DFC37C28FD" as pub state: inlineencodings::GenId;
        /// Marks a printed subtotal row, distinct from an ordinary line item.
        "4265884E340B9600A79E416FFC7E2058" as pub subtotal: inlineencodings::Boolean;
        /// Revolver column `itemlevel`.
        "BECF78E1B535E566A0719CD61A0F5E39" as pub item_level: inlineencodings::I256BE;
    }
}

pub mod code {
    use super::*;
    attributes! {
        /// Vocabulary that defines this code.
        "5D0AFE99D1A0ED02F3C22EF6D041A0CE" as pub domain: inlineencodings::GenId;
        /// Value supplied by the source within its vocabulary.
        "FC218033838285C8B922C78F7A283BD2" as pub code: inlineencodings::ShortString;
    }
}

pub mod party {
    use super::*;
    attributes! {
        /// Revolver column `f_3`.
        "F2477E4236FF6D214B9043275E2E7771" as pub company_name: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `f_5`.
        "6961C833D55666E410CED9DB7CF71A88" as pub last_name: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `f_6`.
        "CEA03E1B46A9EF7C77A7C251E652D5DE" as pub first_name: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `f_11`.
        "4EE1BCC961AAEB3C1DB32BA224B6A0D2" as pub employer: inlineencodings::GenId;
        /// Revolver column `f_12`.
        "735779CF52B0E557EF61C1C061D5EFC5" as pub role: inlineencodings::GenId;
        /// Revolver column `f_62`.
        "BC3E23CC1743897EA781D772AD0375E4" as pub number: inlineencodings::ShortString;
        /// Revolver column `f_31`.
        "939B15A2B942159AA6555DBF2843A533" as pub email: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `f_14`.
        "847C3B105CA09BC460AA83704C3D2FAD" as pub street: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `f_16`.
        "C99A7AE67972C99CBFE79B5BC69D15C8" as pub postal_code: inlineencodings::ShortString;
        /// Revolver column `f_17`.
        "AD330A4E215B893490DD7A5D964A062C" as pub city: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `f_15`.
        "3D910C7E41A5F54AD39AA36B755EC257" as pub country: inlineencodings::ShortString;
        /// Revolver column `f_44`.
        "A3AAEF9604DE11E2AB38385B4C13EC21" as pub vat_id: inlineencodings::ShortString;
        /// Revolver column `f_87`.
        "364EB9C80DAB1A94990180EBC5705F6D" as pub master_address: inlineencodings::GenId;
        /// Revolver column `f_39`.
        "52BEE5CAB9E111E581235926A4389D0B" as pub account_owner: inlineencodings::I256BE;
    }
}

pub mod sales {
    use super::*;
    attributes! {
        /// Revolver column `f_3`.
        "722373B7EA1DFAA3969E6E4C7D07DA2D" as pub number: inlineencodings::ShortString;
        /// Revolver column `f_2`.
        "BB4DD4A6207DA0FFAA1CBBD14B323C95" as pub date: inlineencodings::NsTAIInterval;
        /// Revolver column `f_25`.
        "DFF6475A921719D2E2C63A5EA5FD812A" as pub due_date: inlineencodings::NsTAIInterval;
        /// Revolver column `f_45`.
        "EAB0217520516AF2C71CD8568701CC3A" as pub delivery_date: inlineencodings::NsTAIInterval;
        /// Revolver column `f_1`.
        "831124005DC6950A80DF7CC4DFBFF11E" as pub customer: inlineencodings::GenId;
        /// Revolver column `f_16`.
        "E78D1187B1B8B948DBF53D7D758940F2" as pub attention: inlineencodings::GenId;
        /// Revolver column `f_7`.
        "6EA85DD214C518DD196F525ABD38998C" as pub currency: inlineencodings::GenId;
        /// Revolver column `f_8`.
        "E71665343FF5FB66D2F9AF488B1723FE" as pub net_total: inlineencodings::Currency<Euro>;
        /// Revolver column `f_9`.
        "922C9CB7D1108A83B54E8AF72AB4B66F" as pub tax_total: inlineencodings::Currency<Euro>;
        /// Revolver column `f_10`.
        "4186552C5ADE4D131198025B1DBF214A" as pub gross_total: inlineencodings::Currency<Euro>;
        /// Revolver column `f_17`.
        "A097A5C9F7BE0F1AC82ECB1DE0CBF51E" as pub outstanding: inlineencodings::Currency<Euro>;
        /// Revolver column `f_18`.
        "3464C304CBCD90F738DC52FFB0CC3EA3" as pub customer_reference: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `f_39`. Encoded as an exact proportion.
        "CE912CBD4C68258A76B3404F7E018764" as pub tax_rate_full: inlineencodings::R256BE;
        /// Revolver column `f_40`. Encoded as an exact proportion.
        "AA90807F6AF86A2E7C5AE9A71CCE24A5" as pub tax_rate_reduced: inlineencodings::R256BE;
        /// Revolver column `f_28`.
        "00CC6DE1A91C2C02A1345AE986D85E48" as pub project: inlineencodings::GenId;
        /// Revolver column `f_24`.
        "27FDF9B93DCF380E6DA163F106625C97" as pub query_to: inlineencodings::I256BE;
    }
}

pub mod line {
    use super::*;
    attributes! {
        /// Parent sales document, part of the line identity.
        "9B9119ED1A6D8BCAA84FDDDFA5A2EFC7" as pub document: inlineencodings::GenId;
        /// Revolver column `f_1`.
        "0F4C70BFADFC0FF9D108A92906B9E28A" as pub position_type: inlineencodings::GenId;
        /// Revolver column `numtext`.
        "8C60C352B1A1A9C8FC5229D546422DEC" as pub line_number: inlineencodings::ShortString;
        /// Revolver column `f_12`.
        "2CCAD7886B02DDB21DDD332B80869875" as pub description: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `f_5`.
        "B97216F636CF6C6A021CB11382E12F3F" as pub quantity: inlineencodings::R256BE;
        /// Revolver column `f_6`.
        "892B96BAC9D88D737767B34E93C2D4D0" as pub unit: inlineencodings::ShortString;
        /// Revolver column `f_8`.
        "C4921C82FFDB0609C765CE5288CC4BB4" as pub unit_price: inlineencodings::Currency<Euro>;
        /// Revolver column `f_9`.
        "709EBD1B311E29B8AA09E804FCFBADFA" as pub total: inlineencodings::Currency<Euro>;
        /// Revolver column `f_10`. Encoded as an exact proportion.
        "007DE98150C96596C71C94288E18C760" as pub discount: inlineencodings::R256BE;
        /// Revolver column `f_11`.
        "7930BB88DCCCE2D46D9CEDC4FE496A6C" as pub tax_class: inlineencodings::GenId;
        /// Revolver column `f_19`.
        "DDFF8767E6563BF5DD55FC2D607A8DB2" as pub net_full: inlineencodings::Currency<Euro>;
        /// Revolver column `f_20`.
        "449F5EC3E8DC3DB84DB78AF786A1B1F5" as pub net_reduced: inlineencodings::Currency<Euro>;
        /// Revolver column `f_21`.
        "3179178B5EF414B4A2BE7401F17AAD2A" as pub net_free: inlineencodings::Currency<Euro>;
    }
}

pub mod payment {
    use super::*;
    attributes! {
        /// Revolver column `f_2`.
        "C4D3692AA8270D96E9103BDC764CBE89" as pub date: inlineencodings::NsTAIInterval;
        /// Revolver column `f_4`.
        "6AF5F6EBBBA98BBFA412FF9377993477" as pub amount: inlineencodings::Currency<Euro>;
        /// Revolver column `f_3`.
        "FB01C36E4917E7C7A9B948BEA80F4E34" as pub memo: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `f_10`.
        "A99EE589ED3F9E891A28B7D3BEE547D4" as pub counterparty: inlineencodings::GenId;
        /// Revolver column `f_5`.
        "2D50814D4714AB2543226E48EA1FD285" as pub mode: inlineencodings::GenId;
        /// Revolver column `f_11`.
        "1331BCA5B2CB766B2BF6CA8BFD02BC1E" as pub receipt_number: inlineencodings::Handle<blobencodings::UTF8String>;
    }
}

pub mod allocation {
    use super::*;
    attributes! {
        /// Parent payment, part of the allocation identity.
        "309105063B6B1A826EAA83942272A0C9" as pub payment: inlineencodings::GenId;
        /// Revolver column `f_1`.
        "17A611AC8514E67DBAF75EC76A0CEC27" as pub amount: inlineencodings::Currency<Euro>;
        /// Revolver column `f_2`.
        "1C5362B16E591617CBDA3399715B99A2" as pub sales_document: inlineencodings::GenId;
        /// Revolver column `f_5`.
        "6F7B38A99B9D92BD21C21CBAE19613AC" as pub purchase_document: inlineencodings::GenId;
    }
}

pub mod project {
    use super::*;
    attributes! {
        /// Revolver column `f_2`.
        "AC8DB09A2294233A4014BF80FCC49A56" as pub parent_project: inlineencodings::GenId;
        /// Revolver column `f_3`.
        "9AE57F68EED187BFD53B519526589494" as pub customer: inlineencodings::GenId;
        /// Revolver column `f_8`.
        "4A2BBF13ED543EAB5C3CD5E9D257B75D" as pub contact: inlineencodings::GenId;
        /// Revolver column `f_5`.
        "DE9751DE46657B7DE8D907ECD1CC566C" as pub responsible: inlineencodings::I256BE;
        /// Revolver column `f_9`.
        "D53C9972484554040A3823C184030AD1" as pub number: inlineencodings::ShortString;
        /// Revolver column `f_12`.
        "F5C7E236B4B09BD323205B4FB0CDB660" as pub date: inlineencodings::NsTAIInterval;
        /// Revolver column `f_6`.
        "85DACE246D03333689AD0B6941680B7B" as pub delivery_date: inlineencodings::NsTAIInterval;
        /// Revolver column `f_11`.
        "6FC11E36AEE53A135A5D8D4D745AE2AA" as pub budget: inlineencodings::Currency<Euro>;
        /// Revolver column `f_4`.
        "55CB9887F9CF3249FEEFDB9934AA016C" as pub status: inlineencodings::GenId;
        /// Revolver column `f_7`.
        "8B0E6B9C5BF60017BC9324BE5F21D1BF" as pub description: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `f_14`.
        "F8E7AF7B5A4C52704ACB5AD528F66AF4" as pub customer_reference: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `f_19`.
        "3BB8F2B4596D244AAED58DB1DADB4B69" as pub purchase_total: inlineencodings::Currency<Euro>;
        /// Revolver column `f_21`.
        "0AEB0D1BF4172B517E9A5005D4E29BEB" as pub margin_total: inlineencodings::Currency<Euro>;
        /// Revolver column `f_22`. Encoded as an exact proportion.
        "786BA6854CDD68E3BCFB59465DCB900B" as pub margin_rate: inlineencodings::R256BE;
        /// Revolver column `f_23`.
        "CEFD3618E10BDB942CE7DC96EA78F199" as pub sales_total: inlineencodings::Currency<Euro>;
        /// Revolver column `f_26kqpmejpupqlthxt`.
        "1735CD970D9E71634BA668F4DB8E826E" as pub division: inlineencodings::GenId;
    }
}

pub mod step {
    use super::*;
    attributes! {
        /// Parent project, part of the project step identity.
        "32D618247B6674E572238376D2666E76" as pub project: inlineencodings::GenId;
        /// Revolver column `numtext`.
        "0572F867E7BA41C1D3692CD3BCE03735" as pub line_number: inlineencodings::ShortString;
        /// Revolver column `f_3`.
        "925E612DE3A7A6D0C7DDE14A746DB778" as pub employee: inlineencodings::I256BE;
        /// Revolver column `f_5`.
        "2537093175EE39B3FDB38FD6A68305B4" as pub start_date: inlineencodings::NsTAIInterval;
        /// Revolver column `f_6`.
        "5F5C7EB429BC7829FBC8BC21BA8B682F" as pub end_date: inlineencodings::NsTAIInterval;
        /// Revolver column `f_8`.
        "952F51583DCD608C0C88387E9721886D" as pub notes: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `f_9`.
        "85E8CD16BC1901DD7E60C488718B54D0" as pub done: inlineencodings::Boolean;
        /// Revolver column `f_11`.
        "182B675653E714B3F285909076630B43" as pub service: inlineencodings::GenId;
        /// Revolver column `f_12`.
        "B24E4DF63243DD033D3E8347976C14EB" as pub quantity_planned: inlineencodings::R256BE;
        /// Revolver column `f_13`.
        "E0BC145484D7823F76FC18024D1FBA9B" as pub quantity_actual: inlineencodings::R256BE;
        /// Revolver column `f_14`.
        "3D886D4C43436B63D438D83D771F906A" as pub purchase_total: inlineencodings::Currency<Euro>;
        /// Revolver column `f_16`.
        "5D7098FE52551C16BA6A63F172FA8914" as pub purchase_unit_price: inlineencodings::Currency<Euro>;
        /// Revolver column `f_18`.
        "855789DF8F8D4D01BBBAA0C3EA78D84E" as pub supplier: inlineencodings::GenId;
        /// Revolver column `f_19`. Time since midnight in nanoseconds.
        "61088742AED366A9101BD7D8DF914A3F" as pub start_time: inlineencodings::NsDuration;
        /// Revolver column `f_20`. Time since midnight in nanoseconds.
        "2F691EF1B26EAA1EB2865EBD5A4B67F1" as pub end_time: inlineencodings::NsDuration;
        /// Revolver column `f_22`.
        "202509BC6A4F8CB93F13A97F9F04C4EB" as pub price_tier: inlineencodings::GenId;
        /// Revolver column `f_23`.
        "C9A77B24B9EE782D1F0393EE48F16142" as pub unit: inlineencodings::GenId;
        /// Revolver column `f_24`. Encoded as an exact proportion.
        "F6ACA8624F4AA55D7435B39746C9283E" as pub discount: inlineencodings::R256BE;
        /// Revolver column `f_25`.
        "4E89A69573C216D663EDB4E678CBEA99" as pub tax_class: inlineencodings::GenId;
        /// Revolver column `f_28`.
        "DE664107E7598A87AE6D88FC3BA517F2" as pub billing_mode: inlineencodings::GenId;
        /// Revolver column `f_29`.
        "36B355D3D5106EBA61E2A426350F15EC" as pub billed_in: inlineencodings::GenId;
        /// Revolver column `f_31`.
        "7B15D6317E0137DF1EA18EFE689B0458" as pub margin: inlineencodings::Currency<Euro>;
        /// Revolver column `f_32`. Encoded as an exact proportion.
        "D64AEFC9601832D22E9822C00A52AAC2" as pub margin_rate: inlineencodings::R256BE;
        /// Revolver column `f_33`.
        "D90AFC6AA4EAEB92A30585B94314536A" as pub outsourced: inlineencodings::Boolean;
        /// Revolver column `f_34`.
        "A0338D814D7435047A511325E74C5241" as pub billed: inlineencodings::Boolean;
        /// Revolver column `f_36`.
        "4C69CA22EB99E834274AE6FA02FB7004" as pub sales_unit_price: inlineencodings::Currency<Euro>;
        /// Revolver column `f_37`.
        "8D7183AF159CBE45D9B972785111D569" as pub sales_total: inlineencodings::Currency<Euro>;
        /// Revolver column `f_40`.
        "46BCCBBA34F710D5495ECCC527E3C6F5" as pub quantity_remaining: inlineencodings::R256BE;
        /// Revolver column `f_41`. Encoded as an exact proportion.
        "25A523EEB92B4C761D92412129427144" as pub remaining_rate: inlineencodings::R256BE;
    }
}

pub mod purchase {
    use super::*;
    attributes! {
        /// Revolver column `f_1`.
        "2D01F478685657DE1547E1DD49829395" as pub supplier: inlineencodings::GenId;
        /// Revolver column `f_16`.
        "D650CFC76FD188611AA908239A286CAA" as pub attention: inlineencodings::GenId;
        /// Revolver column `f_2`.
        "E8463CC40C643561FD1C70FE46C4760E" as pub date: inlineencodings::NsTAIInterval;
        /// Revolver column `f_3`.
        "B4F437403E0F18D3FED633052661FCC8" as pub number: inlineencodings::ShortString;
        /// Revolver column `f_8`.
        "0BECEC6695144E88BD19C55FC646A852" as pub net_total: inlineencodings::Currency<Euro>;
        /// Revolver column `f_9`.
        "F90364A922B584C4409DC2B80F633493" as pub tax_total: inlineencodings::Currency<Euro>;
        /// Revolver column `f_10`.
        "42A144ED37608BC3FC8DE4467E94EB36" as pub gross_total: inlineencodings::Currency<Euro>;
        /// Revolver column `f_13`.
        "388DE870DD8422EAC47481582DC784C9" as pub address: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `f_17`.
        "AE62823BFFC224AF9156A7611C49A5D8" as pub outstanding: inlineencodings::Currency<Euro>;
        /// Revolver column `f_18`.
        "EA2F31B567A449D230ED7C94F1C5ECED" as pub external_number: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `f_20`.
        "4C718E9FE902E7C5BA69A0D2C18A5849" as pub delivery_method: inlineencodings::GenId;
        /// Revolver column `f_25`.
        "E0B86E2B9B717BF90DBF877A9B25543C" as pub due_date: inlineencodings::NsTAIInterval;
        /// Revolver column `f_28`.
        "A1891EE1C3EA17DAA84DF5AD3C296A75" as pub cash_purchase: inlineencodings::Boolean;
        /// Revolver column `f_30`.
        "BD2A36D76FAFE4E7EAE5B2C069B8667C" as pub net_full: inlineencodings::Currency<Euro>;
        /// Revolver column `f_31`.
        "5043C97D0AEC6B6A377C5161695EB8B2" as pub net_reduced: inlineencodings::Currency<Euro>;
        /// Revolver column `f_32`.
        "4594C22183DA19331ED261F0F5982C5C" as pub net_free: inlineencodings::Currency<Euro>;
        /// Revolver column `f_33`. Encoded as an exact proportion.
        "09C4BC257CC2E7171E9611583504FADE" as pub tax_rate_full: inlineencodings::R256BE;
        /// Revolver column `f_34`. Encoded as an exact proportion.
        "24D4BE9CC04E440FB5B9FAEEB4030DC0" as pub tax_rate_reduced: inlineencodings::R256BE;
        /// Revolver column `f_35`.
        "E6FA44496EDEBC60DE9E988C4BDF2974" as pub tax_full: inlineencodings::Currency<Euro>;
        /// Revolver column `f_36`.
        "BF0976EC81260CC889836C47AB87A2A0" as pub tax_reduced: inlineencodings::Currency<Euro>;
        /// Revolver column `f_38`.
        "4DA4BFF40F0ECDBFAA426064CA26ABFE" as pub is_delivery_note: inlineencodings::Boolean;
        /// Revolver column `f_45tjuahssckklqthm`.
        "A54B362C0DDCFC6C578437E74A093B62" as pub payment_terms_days: inlineencodings::R256BE;
        /// Revolver column `f_46tjuacyxufskesax`.
        "B51F169DD8450280F6354EBA0B90714F" as pub discount_1_days: inlineencodings::R256BE;
        /// Revolver column `f_47tjuachslcffatyy`.
        "4BB36E1210B9F650ABE890A7E558FE94" as pub discount_2_days: inlineencodings::R256BE;
        /// Revolver column `f_48tjuakpxlapcpuhe`. Encoded as an exact proportion.
        "F63E825C987CD151F7E22F97F494EA78" as pub discount_1_rate: inlineencodings::R256BE;
        /// Revolver column `f_49tjuakhjxhltqpqh`. Encoded as an exact proportion.
        "9B96EF75664014384DC39B2D3F03FDE6" as pub discount_2_rate: inlineencodings::R256BE;
        /// Revolver column `f_52tjsqpmeltlcmptl`.
        "24C6081B395A4B662D01827618DFD407" as pub discount_1_due: inlineencodings::NsTAIInterval;
        /// Revolver column `f_53tjsqptqheysfcxy`.
        "2D9E9CECCF3148EFFC88347CA4B2276E" as pub discount_2_due: inlineencodings::NsTAIInterval;
        /// Revolver column `f_7`.
        "8B93D6C70A4CE7FB0BB6224E221312EC" as pub currency: inlineencodings::GenId;
        /// Revolver column `f_55`.
        "612845C9CB2E8A3C8D997E4B5A97606B" as pub foreign_currency: inlineencodings::Boolean;
        /// Revolver column `f_56`.
        "BAEF67EC8883726EFA4D69F890E61D44" as pub entered_in_foreign: inlineencodings::Boolean;
        /// Revolver column `f_45xlqkqjxucqqkpyj`.
        "BC2C632F52F045886FBCB0C5BA403CD3" as pub division: inlineencodings::GenId;
        /// Revolver column `f_24`.
        "AD5EFC65113F04A7340EBFE9E128CD18" as pub query_to: inlineencodings::I256BE;
        /// Revolver column `f_23`.
        "ABBB40A6E73055B679EB14BD13808154" as pub print_query: inlineencodings::Boolean;
    }
}

pub mod purchase_line {
    use super::*;
    attributes! {
        /// Parent purchase document, part of the line identity.
        "E4FB41B8A470EE85D8ABAB64D289975B" as pub document: inlineencodings::GenId;
        /// Revolver column `numtext`.
        "971866649505206D89EE54941E8E2623" as pub line_number: inlineencodings::ShortString;
        /// Revolver column `f_2`.
        "07780634D38B02E3D6E41D588CB997D7" as pub article: inlineencodings::GenId;
        /// Revolver column `f_4`.
        "D7B78796FAD112CFC6BFFD75CC60A3CA" as pub project: inlineencodings::GenId;
        /// Revolver column `f_5`.
        "3D3138E6B30BE15C38819FA32E198998" as pub quantity: inlineencodings::R256BE;
        /// Revolver column `f_6`.
        "87B96DF5051300378F0FDDA1BEBBAE2D" as pub unit: inlineencodings::ShortString;
        /// Revolver column `f_7`.
        "1885B0470AA1A9365D6E3F93A75F6B5F" as pub price_selection: inlineencodings::GenId;
        /// Revolver column `f_8`.
        "3A105F00257CDD6A04FB49B650EAC962" as pub unit_price: inlineencodings::Currency<Euro>;
        /// Revolver column `f_9`.
        "89C393EFF685E714C85D9C459C1A198A" as pub total: inlineencodings::Currency<Euro>;
        /// Revolver column `f_10`. Encoded as an exact proportion.
        "43539C060414B5F276DC37B68D28D761" as pub discount: inlineencodings::R256BE;
        /// Revolver column `f_11`.
        "E06A7ABF9F662267A81B66610EB50E53" as pub tax_class: inlineencodings::GenId;
        /// Revolver column `f_12`.
        "C63008D3E2769AA24B56EF74EAE781FB" as pub description: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `f_14`.
        "5E806945DBABED1890BD43BE5EF25A9E" as pub quantity_delivered: inlineencodings::R256BE;
        /// Revolver column `f_16`.
        "96F794009E5CBE1BCCFA3B71621F7BA3" as pub net_full: inlineencodings::Currency<Euro>;
        /// Revolver column `f_17`.
        "89EA768DA522DA67BD2B1E71F4C7076C" as pub net_reduced: inlineencodings::Currency<Euro>;
        /// Revolver column `f_18`.
        "380661403D268CB43208F95111C193DE" as pub net_free: inlineencodings::Currency<Euro>;
        /// Revolver column `f_19`.
        "9BF1C5B944A2D4FA091C3793FAA450D6" as pub step_record: inlineencodings::I256BE;
        /// Amount and currency selector read from the optional levy_amount and levy_amount_c bindings.
        "C4E40C5BC123E3A6327B62DDB970E5CC" as pub levy_amount: inlineencodings::Currency<Euro>;
        /// Exact proportion read from the optional levy_rate binding.
        "554EA575267FBC290789AE0F656EB8CD" as pub levy_rate: inlineencodings::R256BE;
    }
}

pub mod time {
    use super::*;
    attributes! {
        /// Revolver column `f_1`.
        "FEEFBFCCBF3B9924A10CDB514A686DD6" as pub employee: inlineencodings::I256BE;
        /// Revolver column `f_2`.
        "8C6628DC539352BADEC881B10E13A1C1" as pub date: inlineencodings::NsTAIInterval;
        /// Revolver column `f_4`.
        "130E4243A70B8A80568E99924BE6D9EC" as pub hourly_rate: inlineencodings::Currency<Euro>;
        /// Revolver column `f_5`.
        "AC4B1277C41E2509C65700BBA919643C" as pub hours: inlineencodings::R256BE;
        /// Revolver column `f_6huucyypxpmjmups`. Time since midnight in nanoseconds.
        "D01694604BB6D055ABD6CA4D92D118ED" as pub work_start: inlineencodings::NsDuration;
        /// Revolver column `f_7huucyfusmfajlcc`. Time since midnight in nanoseconds.
        "767F8EE5479B2F384C8306BD5B282DE2" as pub work_end: inlineencodings::NsDuration;
        /// Revolver column `f_8huucyaclfxlulhf`.
        "56F997FB2D94125DCF1BCFBEB577BE0D" as pub attendance: inlineencodings::R256BE;
    }
}

pub mod booking {
    use super::*;
    attributes! {
        /// Parent time entry, part of the booking identity.
        "A0C2D9A9CEB00AB20ECAA47A2E49EE04" as pub entry: inlineencodings::GenId;
        /// Revolver column `numtext`.
        "EAB53C891CF22CA8EBA229D60C2E12B9" as pub line_number: inlineencodings::ShortString;
        /// Revolver column `f_1`. Time since midnight in nanoseconds.
        "1723EF1B736C25B4BAC24B1D7DC178C4" as pub start: inlineencodings::NsDuration;
        /// Revolver column `f_2`. Time since midnight in nanoseconds.
        "51B292D37D5E7D015499F918B1C4A98C" as pub end: inlineencodings::NsDuration;
        /// Revolver column `f_3`.
        "F44AC7766B1DA5B43E64316CB838D35D" as pub project: inlineencodings::GenId;
        /// Revolver column `f_4`.
        "3524D8C4339F5162DF236156B6692DDC" as pub activity: inlineencodings::GenId;
        /// Revolver column `f_5`.
        "EAD8E492F8DF43BEFA46AA5B52F06350" as pub note: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `f_6`.
        "B4553297220ECB9AC40940D9E50C5AB0" as pub quantity: inlineencodings::R256BE;
        /// Revolver column `f_7`.
        "11284BA479A7BA35613F26E30C2634A7" as pub billable: inlineencodings::Boolean;
        /// Revolver column `f_8`.
        "D2A4E4313264CE68B121BE50E8103324" as pub cost: inlineencodings::Currency<Euro>;
        /// Revolver column `f_9`.
        "2B112D0F046EBEABE82612A28FF1C4D0" as pub unit_price: inlineencodings::Currency<Euro>;
        /// Revolver column `f_10`.
        "2089A4FB1EABABED7C3084749B16F76C" as pub step_record: inlineencodings::I256BE;
        /// Revolver column `f_11`.
        "DE19D17A781A8DD9F1D49D0ABA877965" as pub duration: inlineencodings::R256BE;
        /// Revolver column `f_12huucaftuhxehcyk`.
        "8B2A83482BD1A7D2CFE61DC477315812" as pub customer: inlineencodings::GenId;
        /// Revolver column `f_15cyeekmplymhqphu`.
        "FF3FE8E7C5E6F5B46E44C0325667B206" as pub extra_effort: inlineencodings::Boolean;
    }
}

pub mod article {
    use super::*;
    attributes! {
        /// Revolver column `f_2`.
        "F036A102A78741669E522D221D568B35" as pub code: inlineencodings::ShortString;
        /// Revolver column `f_3`.
        "502B8358F8A0E45763D24722D9C9E119" as pub category: inlineencodings::GenId;
        /// Revolver column `f_4`.
        "85619C7DB70E413F40F1AAA3F681D6C1" as pub group: inlineencodings::GenId;
        /// Revolver column `f_5`.
        "EA76D42C465A9A0D90024B38943EF0A2" as pub unit: inlineencodings::ShortString;
        /// Revolver column `f_7`.
        "0E1519245E54D06846ED5A92694FA27E" as pub description: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `f_13`.
        "96DD38E5F27FCAD6716944D543B22399" as pub price_list_a: inlineencodings::Boolean;
        /// Revolver column `f_27`.
        "4A7604EF395F0477E95BE4CB721B5025" as pub purchase_price: inlineencodings::Currency<Euro>;
        /// Revolver column `f_28`.
        "F4A68A1186BFDAEC77CBE1CA02DBA129" as pub sales_price: inlineencodings::Currency<Euro>;
        /// Revolver column `f_29`.
        "2043DBC9C28624D6C0F6F1285469C71E" as pub packaging_unit: inlineencodings::R256BE;
        /// Revolver column `f_35`.
        "45ADFDEE4D2351257E9840F61F3DBECC" as pub margin: inlineencodings::Currency<Euro>;
        /// Revolver column `f_36`. Encoded as an exact proportion.
        "3BEC7E18DAB7CD43E5DB34C96D76150B" as pub margin_rate: inlineencodings::R256BE;
        /// Revolver column `f_37`.
        "CF4971036675524201CCB3D817F4EC3C" as pub purchase_price_gross: inlineencodings::Currency<Euro>;
        /// Revolver column `f_38`.
        "FB44D9301ED0FA585418A3A67145FEF1" as pub sales_price_gross: inlineencodings::Currency<Euro>;
        /// Revolver column `f_20`.
        "A51A5C96B9E6B6CBCB07BDF67DB238DD" as pub stock: inlineencodings::R256BE;
    }
}

pub mod supply {
    use super::*;
    attributes! {
        /// Article to which these supply terms apply.
        "ACBD3743BA3B1997B1195EA7C50F6D7D" as pub article: inlineencodings::GenId;
        /// Revolver column `f_1`.
        "B7905B19F294FF6E4974CCEC94694A39" as pub supplier: inlineencodings::GenId;
        /// Revolver column `f_5`.
        "F802216EADA6593A3956C23885D560FD" as pub packaging_unit: inlineencodings::R256BE;
        /// Revolver column `f_7`.
        "BAC4FB86B0F98B371422654F2B28885E" as pub unit_price: inlineencodings::Currency<Euro>;
        /// Revolver column `f_9`.
        "E653395DF55E9D10FB0B3F3109FA5CD2" as pub tax_class: inlineencodings::GenId;
    }
}

pub mod price {
    use super::*;
    attributes! {
        /// Article to which this price applies.
        "35E14267282D6C3E68A507953FF8EA5A" as pub article: inlineencodings::GenId;
        /// Revolver column `f_2`.
        "BCDDED539179448C38873F6947E12288" as pub packaging_unit: inlineencodings::R256BE;
        /// Revolver column `f_3`.
        "9EB147C4354B1622848EBA9B2C8F6761" as pub category: inlineencodings::GenId;
        /// Revolver column `f_4`.
        "F0617585DAFF1FF4323637AED7FB9FBF" as pub customer: inlineencodings::GenId;
        /// Revolver column `f_5`.
        "9AC7D39629CAFF8E7DBD78180771BFA5" as pub label: inlineencodings::ShortString;
        /// Revolver column `f_9`.
        "E452AE51077FD6B3C0BA33DA8A8B837C" as pub tax_class: inlineencodings::GenId;
        /// Revolver column `f_11`.
        "F48114F8D9F7397BE463E9B53C49AE32" as pub net_price: inlineencodings::Currency<Euro>;
    }
}

pub mod employee {
    use super::*;
    attributes! {
        /// Revolver column `f_1`.
        "4F648A9CD43A9905564C13F7D08E9B13" as pub user: inlineencodings::I256BE;
        /// Revolver column `f_20`.
        "A3B917E5BFAB8D0DE1182F195FCB3B4B" as pub first_name: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `f_21`.
        "EBE550B9BF809930933F759E6FCE019C" as pub last_name: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `f_18`.
        "13176CAB89297531A3C6783C109B80D0" as pub salutation: inlineencodings::ShortString;
        /// Revolver column `f_13`.
        "B86E4E26AC870DEC40D3D878001C0589" as pub status: inlineencodings::GenId;
        /// Revolver column `f_15`.
        "6A1DE13E207A63EF98D125028095F36B" as pub hired: inlineencodings::NsTAIInterval;
        /// Revolver column `f_16`.
        "C3E26DFE25921E6AA6E911886DC37C6D" as pub left: inlineencodings::NsTAIInterval;
        /// Revolver column `f_29hjmqstfcxkfmsua`.
        "0A9BE08BECE058BFDE9492BEFC9B2E77" as pub role: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `f_30hjmqshqlclppumj`.
        "3272E9FFFCD2B66EDCD44E0C7DB6DF78" as pub department: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `f_31ctqqxtxktkhpujs`.
        "9EF71B05EFE1790F08700B4381CFD707" as pub weekly_hours: inlineencodings::R256BE;
        /// Revolver column `f_26aaaaaaaa`.
        "97B471FE8090A1859C91DB7020FADEF0" as pub annual_leave: inlineencodings::R256BE;
        /// Revolver column `f_28aaaaaaaa`.
        "8BD9A72F4C5E415A9CA2DE89EEDC71CA" as pub leave_entitlement: inlineencodings::R256BE;

        /// Revolver column `f_2`.
        "BAA1EEE2826FB04E29D3CC221EADCF0D" as pub street: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `f_4`.
        "8B9AF65F918F61A32DDB29FD1A4E96E2" as pub postal_code: inlineencodings::ShortString;
        /// Revolver column `f_5`.
        "7565D146B48053B89D60AFC04A69968D" as pub city: inlineencodings::ShortString;
        /// Revolver column `f_3`.
        "D4A992C90C0E7F5C04AE7F4E7692FB3C" as pub country: inlineencodings::ShortString;

        /// Revolver column `f_9`.
        "416D8BDB8F0FE4A69101816D57ABBF30" as pub phone: inlineencodings::ShortString;
        /// Revolver column `f_10`.
        "02A12D1D5DD17634840D99DBA9B65599" as pub mobile: inlineencodings::ShortString;

        /// Revolver column `f_11`.
        "66EF559DB55B9E0D067170E123D329C9" as pub religion: inlineencodings::ShortString;
        /// Revolver column `f_17`.
        "5A433EEC385939E4F6A4A9AB53BAA583" as pub health_insurer: inlineencodings::ShortString;
        /// Revolver column `f_14`.
        "96304E287221B5F522D80507112C2DAD" as pub social_security_number: inlineencodings::ShortString;
        /// Revolver column `f_6`.
        "96A9E19338AE57EBD190CBE4130380D2" as pub marital_status: inlineencodings::GenId;
        /// Revolver column `f_7`.
        "C52CDC3D507808F1FE4CA1A62CEF72F4" as pub tax_class: inlineencodings::GenId;

        /// Revolver column `f_23`.
        "294C72D40893CBD55DF9D72E234E5F22" as pub hourly_rate: inlineencodings::Currency<Euro>;
    }
}

pub mod user {
    use super::*;
    attributes! {
        /// Revolver column `userid`.
        "664EA59453C4215B1E9CD364BEE68BF5" as pub userid: inlineencodings::I256BE;
        /// Revolver column `first_name`.
        "3835B671DE8C9EDE4D90E30BF8EDE01A" as pub first_name: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `last_name`.
        "504C07DCD6C44EE910293A323363C03D" as pub last_name: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `initials`.
        "6FF7B1664234B1F71AB35ACC87D3B4CF" as pub initials: inlineencodings::ShortString;
    }
}

pub mod opportunity {
    use super::*;
    attributes! {
        /// Revolver column `f_1`.
        "3DC2503085B28BA4B11787D92A25B85F" as pub company: inlineencodings::GenId;
        /// Revolver column `f_2`.
        "350919301A005201662D2D0BF601220F" as pub contact: inlineencodings::GenId;
        /// Revolver column `f_4`.
        "2DCE2AD3C9D03427B1E79309E1E25AA2" as pub volume: inlineencodings::Currency<Euro>;
        /// Revolver column `f_5`. Encoded as an exact proportion.
        "000303265C66F4EF27684D4BA23D07C4" as pub probability: inlineencodings::R256BE;
        /// Revolver column `f_6`.
        "7FE13F9010E28438FFB6D8D2A3CDEFE1" as pub description: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `f_8`.
        "F2F2440134339EB579EBB79BD47FF632" as pub status: inlineencodings::GenId;
        /// Revolver column `f_9`.
        "08ABD4B4F347CB5ADEBF65B3D5A909DD" as pub next_step: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `f_10`.
        "9D34DB3EB4FE0FA4E08CD3FA08BE4B40" as pub next_step_date: inlineencodings::NsTAIInterval;
        /// Revolver column `f_11`.
        "B46DC612581F46E1AE93E9C48E1D9553" as pub expected_close: inlineencodings::NsTAIInterval;
        /// Revolver column `f_12`.
        "CB46DAD9DB84BDCD0873AAED11597342" as pub source: inlineencodings::GenId;
        /// Revolver column `f_13`.
        "DA96D14A7B7672A4F77AA535727009B2" as pub weighted_volume: inlineencodings::Currency<Euro>;
        /// Revolver column `f_16`.
        "0309A865AB3E9409B0766DA20A3BC7D2" as pub next_step_type: inlineencodings::GenId;
    }
}

pub mod opportunity_step {
    use super::*;
    attributes! {
        /// Opportunity to which this step belongs.
        "0CBB8B977144AF84A46601D82E388FD7" as pub opportunity: inlineencodings::GenId;
        /// Revolver column `numtext`.
        "173140B986CF44AD8538CE70709C5125" as pub line_number: inlineencodings::ShortString;
        /// Revolver column `f_2`.
        "35F78811170F58110609FE7C795F122A" as pub notes: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `f_3`.
        "89F90F662F3494E9D1ABECA453B55007" as pub category: inlineencodings::GenId;
        /// Revolver column `f_4`.
        "EC10D0457C03307BCBA9C884BA01A423" as pub date: inlineencodings::NsTAIInterval;
        /// Revolver column `f_5`.
        "DD48CBDC7E927F925B1367573FCEEAF3" as pub done: inlineencodings::Boolean;
    }
}

pub mod activity {
    use super::*;
    attributes! {
        /// Revolver column `f_2`.
        "4E0C0E7FD3A1913CF8C9CF881F52F801" as pub party: inlineencodings::GenId;
        /// Revolver column `f_3`.
        "60D284E7FFC889CCB02D6F577B4F1D6C" as pub project: inlineencodings::GenId;
        /// Revolver column `f_5`.
        "DC82EBAA193898E1E3FB15BD414AA9DA" as pub category: inlineencodings::GenId;
        /// Revolver column `f_1`.
        "496E5417C2490167D9CBB8945DAD0F84" as pub date_from: inlineencodings::NsTAIInterval;
        /// Revolver column `f_39tjmpxlkkcxeeqkq`.
        "34436D024549E10A4C618DD900E39D02" as pub date_to: inlineencodings::NsTAIInterval;
        /// Revolver column `f_6`. Time since midnight in nanoseconds.
        "08C69F8BC8CD549CFABF14CB985C8478" as pub time_from: inlineencodings::NsDuration;
        /// Revolver column `f_7`. Time since midnight in nanoseconds.
        "993F053BEBB5C9F5F767F7B46A61D0C2" as pub time_to: inlineencodings::NsDuration;
        /// Revolver column `f_18`.
        "D8B9AACBACD7001BEE828B3BCBDABA48" as pub text: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `f_20`.
        "96BEB53373AFB0C273023E4C395E1A74" as pub address: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `f_13`.
        "5093D24116F9CBF040C77E6963F58BCA" as pub query_to: inlineencodings::I256BE;
        /// Revolver column `f_25`.
        "D70DD544A840CEE8B397840F9F889E91" as pub company: inlineencodings::GenId;
        /// Revolver column `f_31`.
        "C82E79EE7DB15EE31F7B32E7A7519396" as pub status: inlineencodings::GenId;
        /// Revolver column `f_11`.
        "B5ADC1437A26277D4518BAC74D6CD6AF" as pub include_project: inlineencodings::Boolean;
        /// Revolver column `f_12`.
        "FEA56D3B098F2F0D1F469163FCEB81AC" as pub include_external_reference: inlineencodings::Boolean;
        /// Revolver column `f_28`.
        "2C407673D5B28C13108C3B82F482D57B" as pub print_query: inlineencodings::Boolean;
    }
}

pub mod request {
    use super::*;
    attributes! {
        /// Revolver column `f_3hjmqjxucmlaqmmx`.
        "49CE72F7C9B8E64495EA4A77FA594B32" as pub first_day: inlineencodings::NsTAIInterval;
        /// Revolver column `f_4hjmqjssceashcfs`.
        "A2319C4610FCB92AFF1A225A9E901B0A" as pub last_day: inlineencodings::NsTAIInterval;
        /// Revolver column `f_5hjmqjlhhphxexqp`.
        "90AEA5B4C4C35A730B2FD8666E256314" as pub days: inlineencodings::R256BE;
        /// Revolver column `f_6hjmqxpmkpxuepsy`.
        "28ED9B4756C706336B68CAA139FF9AA4" as pub notes: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `f_7hjmeqtskutlfepp`.
        "175BCCAD3446EA2B20648C8C484CEAA2" as pub unpaid: inlineencodings::Boolean;
        /// Revolver column `f_11hjmemsejmmjmlek`.
        "59D9DE3F453B8D8653FDE30D4FAB66E5" as pub document_type: inlineencodings::GenId;
        /// Revolver column `f_12hjmetjplkyyfmmc`.
        "23FFC595F1B087201F049DA6869B52CA" as pub applied: inlineencodings::NsTAIInterval;
        /// Revolver column `f_13hjmekfacqjtjphc`.
        "CEBE7B2ABA1EA2144308BB47BC9EC346" as pub employee: inlineencodings::I256BE;
    }
}

pub mod task {
    use super::*;
    attributes! {
        /// Revolver column `f_2`.
        "414DA24A1B23C53DD38EB6A3933CBFBA" as pub project: inlineencodings::GenId;
        /// Revolver column `f_3`.
        "4CB3A97548A57B7647E629109A8B0A0E" as pub parent_task: inlineencodings::GenId;
        /// Revolver column `f_4`.
        "3B1F7850254BBEA2DD22D029046DEAB3" as pub link: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `f_6`.
        "867EF05605AFEB038B206AD489647323" as pub due_date: inlineencodings::NsTAIInterval;
        /// Revolver column `f_8`.
        "4170499D8F8173DF21F64DA5D0A20A8F" as pub followup_date: inlineencodings::NsTAIInterval;
        /// Revolver column `f_9`.
        "786770486BDB764C7099613255F74F9B" as pub note: inlineencodings::Handle<blobencodings::UTF8String>;
        /// Revolver column `f_10culeqhshhuacpcj`.
        "9BA68CFC911B573906D71566BA60F018" as pub category: inlineencodings::GenId;
        /// Revolver column `f_11qfysltppjmyhhxqp`.
        "6DD1329B0C5C866F51F97806A1FA26E9" as pub customer: inlineencodings::GenId;
        /// Party reference read from the optional task_contact binding.
        "1B8A5CD57EDCE0A94249CCD3F9D7C74F" as pub contact: inlineencodings::GenId;
        /// Step record number read from the optional task_step binding.
        "2A5B81C3D0C22EAF97EE03C3E3C5013F" as pub step_record: inlineencodings::I256BE;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use triblespace::core::inline::encodings::money::UsDollar;
    use triblespace::core::inline::{TryFromInline, TryToInline};
    use triblespace::core::metadata::MetaDescribe;

    #[test]
    fn a_revolver_amount_round_trips_exactly() {
        let amount = Amount::<Euro>::from_units(119_000, 3).expect("valid scale");
        assert_eq!(amount.to_string(), "119.00 EUR");
        let decoded = Amount::<Euro>::try_from_inline(&amount.try_to_inline().expect("in domain"))
            .expect("decodes");
        assert_eq!(decoded, amount);
    }

    #[test]
    fn a_sub_cent_amount_is_a_number_here_not_an_exception() {
        let cost = Amount::<Euro>::from_units(11_833, 3).expect("valid scale");
        assert_eq!(cost.to_string(), "11.833 EUR");
        assert_eq!(cost.ratio(), num_rational::Ratio::new(11_833, 1_000));
        let decoded =
            Amount::<Euro>::try_from_inline(&cost.try_to_inline().expect("in domain")).unwrap();
        assert_eq!(decoded, cost);
    }

    #[test]
    fn equal_amounts_encode_identically_with_no_scale_to_agree_on() {
        let from_scale_3 = Amount::<Euro>::from_units(11_900, 3).unwrap();
        let from_scale_2 = Amount::<Euro>::from_units(1_190, 2).unwrap();
        let from_cents = Amount::<Euro>::from_minor(1_190).unwrap();
        assert_eq!(from_scale_3, from_scale_2);
        assert_eq!(from_scale_3, from_cents);
        assert_eq!(
            from_scale_3.try_to_inline().unwrap().raw,
            from_cents.try_to_inline().unwrap().raw
        );
    }

    #[test]
    fn euro_amounts_sort_numerically_by_their_raw_bytes() {
        let raw = |units: i128| {
            Amount::<Euro>::from_units(units, 3)
                .expect("valid scale")
                .try_to_inline()
                .expect("in domain")
                .raw
        };
        assert!(raw(-1_000) < raw(0));
        assert!(raw(0) < raw(1));
        assert!(raw(1) < raw(1_000));
        assert!(raw(11_900) < raw(119_000));
    }

    #[test]
    fn the_currency_is_the_attribute_so_two_currencies_cannot_be_summed() {
        assert_ne!(
            inlineencodings::Currency::<Euro>::id(),
            inlineencodings::Currency::<UsDollar>::id(),
            "one encoding per currency"
        );
        assert_ne!(
            sales::gross_total.id(),
            triblespace::prelude::Attribute::<inlineencodings::Currency<UsDollar>>::anchored(
                id_hex!("4186552C5ADE4D131198025B1DBF214A")
            )
            .id(),
            "the same anchor under another currency is another attribute"
        );
    }
}
