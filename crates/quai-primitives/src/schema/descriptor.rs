// Schema metadata for bounded preflight before allocating message trees.
use prost::Message;
pub const FIELDS: &[&[(u32, u8, Option<usize>)]] = &[
    &[(1, 2, None)],
    &[(1, 2, None)],
    &[(1, 2, Some(1))],
    &[(1, 2, None)],
    &[(1, 0, None)],
    &[(1, 2, Some(0))],
    &[
        (1, 2, Some(1)),
        (2, 2, Some(1)),
        (3, 2, Some(1)),
        (4, 2, Some(1)),
        (5, 2, Some(1)),
        (6, 2, Some(1)),
        (7, 2, Some(1)),
        (8, 2, Some(1)),
        (9, 2, None),
        (10, 2, None),
        (11, 2, None),
        (12, 2, None),
        (13, 2, None),
        (14, 2, None),
        (15, 0, None),
        (16, 0, None),
        (17, 2, None),
        (18, 2, Some(0)),
        (19, 2, None),
        (20, 2, Some(1)),
        (21, 0, None),
        (22, 2, Some(1)),
        (23, 2, Some(1)),
        (24, 0, None),
        (25, 0, None),
        (26, 0, None),
        (27, 2, Some(1)),
        (28, 2, Some(1)),
        (29, 2, Some(1)),
        (30, 0, None),
        (31, 0, None),
        (32, 2, None),
        (33, 2, None),
        (35, 2, None),
        (36, 2, None),
        (37, 2, None),
        (38, 2, None),
        (39, 2, None),
        (40, 2, Some(1)),
        (41, 2, Some(1)),
    ],
    &[
        (1, 0, None),
        (2, 2, None),
        (3, 0, None),
        (4, 2, None),
        (5, 0, None),
        (6, 2, None),
        (7, 2, None),
        (8, 2, None),
        (9, 2, Some(11)),
        (10, 2, None),
        (11, 2, None),
        (12, 2, None),
        (13, 2, Some(1)),
        (14, 0, None),
        (15, 2, Some(34)),
        (16, 2, Some(35)),
        (17, 2, None),
        (18, 2, None),
        (19, 2, Some(1)),
        (20, 2, Some(1)),
        (21, 0, None),
        (22, 0, None),
    ],
    &[(1, 2, Some(7))],
    &[(1, 2, Some(6))],
    &[(1, 2, Some(1))],
    &[(1, 2, Some(24))],
    &[
        (1, 0, None),
        (2, 2, None),
        (3, 2, None),
        (4, 2, None),
        (5, 2, None),
        (6, 0, None),
        (7, 2, None),
    ],
    &[
        (1, 0, None),
        (2, 2, None),
        (3, 0, None),
        (4, 0, None),
        (5, 0, None),
        (6, 0, None),
        (7, 2, None),
        (8, 2, None),
        (9, 2, None),
        (10, 2, None),
    ],
    &[(1, 2, None), (2, 2, None), (3, 2, None)],
    &[
        (1, 2, Some(1)),
        (2, 2, Some(1)),
        (3, 2, None),
        (4, 2, None),
        (5, 2, Some(1)),
        (6, 0, None),
        (7, 2, Some(0)),
        (8, 2, Some(1)),
        (9, 0, None),
        (10, 2, None),
        (11, 0, None),
        (12, 2, Some(3)),
        (13, 2, None),
        (14, 2, Some(12)),
        (15, 2, Some(14)),
        (16, 2, Some(14)),
        (17, 2, None),
        (18, 2, None),
        (19, 2, None),
    ],
    &[(1, 2, Some(15))],
    &[
        (1, 2, Some(6)),
        (2, 2, Some(8)),
        (3, 2, Some(16)),
        (4, 2, Some(8)),
        (5, 2, Some(10)),
        (6, 2, Some(2)),
    ],
    &[(1, 2, Some(15)), (2, 2, Some(17)), (3, 2, Some(7))],
    &[(1, 2, Some(18))],
    &[(1, 2, Some(18))],
    &[(1, 2, Some(20))],
    &[(1, 2, Some(18))],
    &[(1, 2, Some(18))],
    &[(1, 2, None), (2, 2, Some(1))],
    &[
        (1, 2, None),
        (2, 0, None),
        (3, 2, Some(28)),
        (4, 2, Some(1)),
        (5, 2, Some(3)),
        (6, 0, None),
        (7, 2, Some(8)),
    ],
    &[(1, 2, Some(25))],
    &[(1, 2, Some(3)), (2, 2, Some(1)), (3, 2, None)],
    &[(1, 2, Some(27))],
    &[(1, 2, Some(18)), (2, 2, Some(30))],
    &[(1, 2, Some(1)), (2, 2, Some(1))],
    &[(1, 2, None)],
    &[(1, 2, Some(18)), (2, 2, Some(8))],
    &[(1, 2, Some(18)), (2, 2, Some(8))],
    &[(1, 2, Some(36))],
    &[(1, 2, Some(38))],
    &[(1, 2, Some(37)), (2, 2, None)],
    &[(1, 2, Some(1)), (2, 0, None)],
    &[(1, 0, None), (2, 2, None), (3, 2, None)],
    &[(1, 2, Some(1)), (2, 0, None), (3, 0, None), (4, 2, None)],
    &[(1, 2, Some(39))],
    &[(1, 2, Some(37)), (2, 2, Some(38))],
    &[(1, 2, Some(41))],
    &[(1, 2, None)],
    &[(1, 2, None), (2, 2, None)],
    &[(1, 2, Some(44))],
    &[(1, 2, Some(47))],
    &[(1, 0, None), (2, 0, None)],
    &[(1, 2, Some(49))],
    &[(1, 2, Some(50))],
    &[(1, 0, None), (2, 0, None), (3, 2, None)],
    &[(1, 2, None), (2, 2, None)],
    &[(1, 2, None), (2, 0, None), (3, 2, Some(3))],
    &[(1, 2, None), (2, 2, Some(3))],
    &[(1, 2, Some(53))],
    &[(1, 2, None)],
    &[(1, 2, Some(52))],
    &[
        (1, 2, None),
        (2, 2, None),
        (3, 2, None),
        (4, 2, None),
        (5, 2, None),
        (6, 2, None),
    ],
    &[(1, 0, None)],
    &[(1, 2, Some(1)), (2, 0, None), (3, 0, None)],
    &[(1, 2, Some(18))],
    &[(1, 2, Some(7))],
    &[(1, 2, Some(13))],
    &[
        (1, 0, None),
        (2, 2, Some(0)),
        (3, 2, Some(1)),
        (4, 2, None),
        (5, 2, Some(20)),
        (6, 2, Some(21)),
        (7, 2, Some(22)),
        (8, 2, Some(1)),
        (9, 2, Some(13)),
    ],
    &[
        (1, 0, None),
        (2, 2, Some(0)),
        (3, 2, Some(22)),
        (4, 2, Some(20)),
        (5, 2, Some(21)),
        (6, 2, Some(1)),
        (7, 2, Some(13)),
    ],
    &[(1, 2, Some(63)), (2, 2, Some(64))],
    &[(1, 2, Some(67)), (2, 2, None), (3, 0, None), (4, 0, None)],
    &[(1, 2, None), (2, 2, None)],
];
pub const NAMES: &[&str] = &[
    ".common.ProtoLocation",
    ".common.ProtoHash",
    ".common.ProtoHashes",
    ".common.ProtoAddress",
    ".common.ProtoNumber",
    ".common.ProtoLocations",
    ".block.ProtoHeader",
    ".block.ProtoTransaction",
    ".block.ProtoTransactions",
    ".block.ProtoHeaders",
    ".block.ProtoManifest",
    ".block.ProtoAccessList",
    ".block.ProtoAuxPow",
    ".block.ProtoAuxTemplate",
    ".block.ProtoPowShareDiffAndCount",
    ".block.ProtoWorkObjectHeader",
    ".block.ProtoWorkObjectHeaders",
    ".block.ProtoWorkObjectBody",
    ".block.ProtoWorkObject",
    ".block.ProtoWorkObjects",
    ".block.ProtoWorkObjectBlockView",
    ".block.ProtoWorkObjectBlocksView",
    ".block.ProtoWorkObjectHeaderView",
    ".block.ProtoWorkObjectShareView",
    ".block.ProtoAccessTuple",
    ".block.ProtoReceiptForStorage",
    ".block.ProtoReceiptsForStorage",
    ".block.ProtoLogForStorage",
    ".block.ProtoLogsForStorage",
    ".block.ProtoPendingHeader",
    ".block.ProtoTermini",
    ".block.ProtoEtxSet",
    ".block.ProtoPendingEtxs",
    ".block.ProtoPendingEtxsRollup",
    ".block.ProtoTxIns",
    ".block.ProtoTxOuts",
    ".block.ProtoTxIn",
    ".block.ProtoOutPoint",
    ".block.ProtoTxOut",
    ".block.ProtoOutPointAndDenomination",
    ".block.ProtoAddressOutPoints",
    ".block.ProtoSpentUTXO",
    ".block.ProtoSpentUTXOs",
    ".block.ProtoKeys",
    ".block.ProtoKeyValue",
    ".block.ProtoKeysAndValues",
    ".block.ProtoTrimDepths",
    ".block.ProtoTrimDepths.TrimDepthsEntry",
    ".block.ProtoTokenChoiceSet",
    ".block.ProtoTokenChoiceArray",
    ".block.ProtoTokenChoice",
    ".block.ProtoBetas",
    ".block.ProtoLockup",
    ".block.ProtoUnlock",
    ".block.ProtoUnlocks",
    ".block.ProtoLockedBalance",
    ".block.ProtoLockups",
    ".block.ProtoSupplyAnalytics",
    ".db.ProtoNumber",
    ".db.ProtoLegacyTxLookupEntry",
    ".quaiprotocol.GossipWorkObject",
    ".quaiprotocol.GossipTransaction",
    ".quaiprotocol.GossipAuxTemplate",
    ".quaiprotocol.QuaiRequestMessage",
    ".quaiprotocol.QuaiResponseMessage",
    ".quaiprotocol.QuaiMessage",
    ".peerdb.ProtoPeerInfo",
    ".peerdb.ProtoAddrInfo",
];
pub fn canonicalize(name: &str, bytes: &[u8]) -> Result<Vec<u8>, String> {
    super::preflight(name, bytes)?;
    Ok(match name {
        ".common.ProtoLocation" => super::common::ProtoLocation::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".common.ProtoHash" => super::common::ProtoHash::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".common.ProtoHashes" => super::common::ProtoHashes::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".common.ProtoAddress" => super::common::ProtoAddress::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".common.ProtoNumber" => super::common::ProtoNumber::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".common.ProtoLocations" => super::common::ProtoLocations::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoHeader" => super::block::ProtoHeader::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoTransaction" => super::block::ProtoTransaction::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoTransactions" => super::block::ProtoTransactions::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoHeaders" => super::block::ProtoHeaders::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoManifest" => super::block::ProtoManifest::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoAccessList" => super::block::ProtoAccessList::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoAuxPow" => super::block::ProtoAuxPow::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoAuxTemplate" => super::block::ProtoAuxTemplate::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoPowShareDiffAndCount" => {
            super::block::ProtoPowShareDiffAndCount::decode(bytes)
                .map_err(|e| e.to_string())?
                .encode_to_vec()
        }
        ".block.ProtoWorkObjectHeader" => super::block::ProtoWorkObjectHeader::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoWorkObjectHeaders" => super::block::ProtoWorkObjectHeaders::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoWorkObjectBody" => super::block::ProtoWorkObjectBody::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoWorkObject" => super::block::ProtoWorkObject::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoWorkObjects" => super::block::ProtoWorkObjects::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoWorkObjectBlockView" => super::block::ProtoWorkObjectBlockView::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoWorkObjectBlocksView" => {
            super::block::ProtoWorkObjectBlocksView::decode(bytes)
                .map_err(|e| e.to_string())?
                .encode_to_vec()
        }
        ".block.ProtoWorkObjectHeaderView" => {
            super::block::ProtoWorkObjectHeaderView::decode(bytes)
                .map_err(|e| e.to_string())?
                .encode_to_vec()
        }
        ".block.ProtoWorkObjectShareView" => super::block::ProtoWorkObjectShareView::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoAccessTuple" => super::block::ProtoAccessTuple::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoReceiptForStorage" => super::block::ProtoReceiptForStorage::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoReceiptsForStorage" => super::block::ProtoReceiptsForStorage::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoLogForStorage" => super::block::ProtoLogForStorage::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoLogsForStorage" => super::block::ProtoLogsForStorage::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoPendingHeader" => super::block::ProtoPendingHeader::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoTermini" => super::block::ProtoTermini::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoEtxSet" => super::block::ProtoEtxSet::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoPendingEtxs" => super::block::ProtoPendingEtxs::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoPendingEtxsRollup" => super::block::ProtoPendingEtxsRollup::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoTxIns" => super::block::ProtoTxIns::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoTxOuts" => super::block::ProtoTxOuts::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoTxIn" => super::block::ProtoTxIn::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoOutPoint" => super::block::ProtoOutPoint::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoTxOut" => super::block::ProtoTxOut::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoOutPointAndDenomination" => {
            super::block::ProtoOutPointAndDenomination::decode(bytes)
                .map_err(|e| e.to_string())?
                .encode_to_vec()
        }
        ".block.ProtoAddressOutPoints" => super::block::ProtoAddressOutPoints::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoSpentUTXO" => super::block::ProtoSpentUtxo::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoSpentUTXOs" => super::block::ProtoSpentUtxOs::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoKeys" => super::block::ProtoKeys::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoKeyValue" => super::block::ProtoKeyValue::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoKeysAndValues" => super::block::ProtoKeysAndValues::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoTrimDepths" => super::block::ProtoTrimDepths::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoTokenChoiceSet" => super::block::ProtoTokenChoiceSet::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoTokenChoiceArray" => super::block::ProtoTokenChoiceArray::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoTokenChoice" => super::block::ProtoTokenChoice::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoBetas" => super::block::ProtoBetas::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoLockup" => super::block::ProtoLockup::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoUnlock" => super::block::ProtoUnlock::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoUnlocks" => super::block::ProtoUnlocks::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoLockedBalance" => super::block::ProtoLockedBalance::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoLockups" => super::block::ProtoLockups::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".block.ProtoSupplyAnalytics" => super::block::ProtoSupplyAnalytics::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".db.ProtoNumber" => super::db::ProtoNumber::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".db.ProtoLegacyTxLookupEntry" => super::db::ProtoLegacyTxLookupEntry::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".quaiprotocol.GossipWorkObject" => super::quaiprotocol::GossipWorkObject::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".quaiprotocol.GossipTransaction" => super::quaiprotocol::GossipTransaction::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".quaiprotocol.GossipAuxTemplate" => super::quaiprotocol::GossipAuxTemplate::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".quaiprotocol.QuaiRequestMessage" => {
            super::quaiprotocol::QuaiRequestMessage::decode(bytes)
                .map_err(|e| e.to_string())?
                .encode_to_vec()
        }
        ".quaiprotocol.QuaiResponseMessage" => {
            super::quaiprotocol::QuaiResponseMessage::decode(bytes)
                .map_err(|e| e.to_string())?
                .encode_to_vec()
        }
        ".quaiprotocol.QuaiMessage" => super::quaiprotocol::QuaiMessage::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".peerdb.ProtoPeerInfo" => super::peerdb::ProtoPeerInfo::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        ".peerdb.ProtoAddrInfo" => super::peerdb::ProtoAddrInfo::decode(bytes)
            .map_err(|e| e.to_string())?
            .encode_to_vec(),
        _ => return Err("unsupported public message type".into()),
    })
}
