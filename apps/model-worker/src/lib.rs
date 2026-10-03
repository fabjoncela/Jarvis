#![forbid(unsafe_code)]

//! Safe request handler for the contained model-worker shell. It currently
//! returns only `no_action` after strict decoding; inference is not enabled.

use jarvis_model_protocol::{decode_planning_request, encode_no_action_response, RequestError};

pub fn handle_request(bytes: &[u8]) -> Result<Vec<u8>, RequestError> {
    let request = decode_planning_request(bytes)?;
    Ok(encode_no_action_response(&request))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jarvis_core::{ProfileGeneration, RequestContext, RequestId};
    use jarvis_model_protocol::{encode_planning_request, parse_model_proposal, ModelProposal};

    #[test]
    fn shell_never_proposes_an_action_even_for_an_action_request() {
        let request = RequestContext::new(RequestId::next(), ProfileGeneration::initial());
        let encoded = encode_planning_request("open discord another way", request).unwrap();
        let response = handle_request(&encoded).unwrap();
        assert_eq!(
            parse_model_proposal(&response, request),
            Ok(ModelProposal::NoAction)
        );
        assert_eq!(handle_request(b"not-json"), Err(RequestError::Malformed));
    }
}
