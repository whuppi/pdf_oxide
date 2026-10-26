// Digital-signature ops (the signatures capability): signing plus
// signature reads on an open document.

use crate::host::binary_codec::ResponseWriter;
use crate::host::bridge_api::{handle_sign, handle_with_doc};
use crate::host::dispatch;
use crate::host::ops::op_unit;

op_unit!(SIGN, "sign", pdf_op_sign_anchor, |ctx| {
    let source = ctx.take_source(0);
    let sink = ctx.take_sink(0);
    handle_sign(ctx.req, source, ctx.source_bytes, sink)
});

op_unit!(
    GET_SIGNATURES,
    "getSignatures",
    pdf_op_get_signatures_anchor,
    |ctx| {
        handle_with_doc(ctx.state, ctx.req, |doc, _| {
            let result = dispatch::get_signatures(doc)?;
            let mut w = ResponseWriter::ok();
            w.put_map_list("signatures", result.signatures.len(), |i, item| {
                let s = &result.signatures[i];
                item.put_str("signerName", &s.signer_name);
                item.put_str("reason", &s.reason);
                item.put_str("location", &s.location);
                item.put_bool("isValid", s.is_valid);
                item.put_bool("coversWholeDocument", s.covers_whole_document);
                if let Some(secs) = s.signing_time {
                    item.put_i64("signingTime", secs);
                }
            });
            Ok(w.finish())
        })
    }
);

op_unit!(
    VERIFY_SIGNATURES,
    "verifySignatures",
    pdf_op_verify_signatures_anchor,
    |ctx| {
        handle_with_doc(ctx.state, ctx.req, |doc, _| {
            let result = dispatch::verify_signatures(doc)?;
            let mut w = ResponseWriter::ok();
            w.put_bool("valid", result);
            Ok(w.finish())
        })
    }
);
