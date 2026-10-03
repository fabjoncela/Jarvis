#![forbid(unsafe_code)]

//! Safe request handler for the contained model-worker shell. It currently
//! returns only `no_action` after strict decoding; inference is not enabled.

use jarvis_model_protocol::{
    decode_planning_request, encode_frame, encode_no_action_response, FrameDecoder, FrameError,
    FrameKind, RequestError,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FramedRequestError {
    Frame(FrameError),
    Request(RequestError),
}

pub fn handle_request(bytes: &[u8]) -> Result<Vec<u8>, RequestError> {
    let request = decode_planning_request(bytes)?;
    Ok(encode_no_action_response(&request))
}

/// One complete transport message, not an open pipe or a running model.
/// The response remains a strict `no_action` regardless of request wording.
pub fn handle_framed_request(message: &[u8]) -> Result<Vec<u8>, FramedRequestError> {
    let mut decoder = FrameDecoder::new(FrameKind::Request);
    decoder.feed(message).map_err(FramedRequestError::Frame)?;
    let request = decoder.finish().map_err(FramedRequestError::Frame)?;
    let response = handle_request(&request).map_err(FramedRequestError::Request)?;
    encode_frame(FrameKind::Response, &response).map_err(FramedRequestError::Frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jarvis_core::{ProfileGeneration, RequestContext, RequestId};
    use jarvis_model_protocol::{
        encode_planning_request, parse_model_proposal, FrameDecoder, FrameKind, ModelProposal,
    };

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

    #[test]
    fn framed_exchange_is_no_action_and_rejects_extra_or_bad_payload() {
        let request = RequestContext::new(RequestId::next(), ProfileGeneration::initial());
        let encoded = encode_planning_request("open discord another way", request).unwrap();
        let frame = encode_frame(FrameKind::Request, &encoded).unwrap();
        let reply = handle_framed_request(&frame).unwrap();
        let mut decoder = FrameDecoder::new(FrameKind::Response);
        decoder.feed(&reply).unwrap();
        assert_eq!(
            parse_model_proposal(&decoder.finish().unwrap(), request),
            Ok(ModelProposal::NoAction)
        );
        assert_eq!(
            handle_framed_request(&[frame.as_slice(), b"extra"].concat()),
            Err(FramedRequestError::Frame(FrameError::TrailingData))
        );
        assert_eq!(
            handle_framed_request(&frame[..frame.len() - 1]),
            Err(FramedRequestError::Frame(FrameError::Incomplete))
        );
        let malformed = encode_frame(FrameKind::Request, b"not-json").unwrap();
        assert_eq!(
            handle_framed_request(&malformed),
            Err(FramedRequestError::Request(RequestError::Malformed))
        );
    }
}
