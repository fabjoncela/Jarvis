//! Dormant I/O-task seam for one local-model exchange. This is deliberately
//! not called from the resident monitor until an authenticated, cancellable
//! worker-process transport exists.

use std::time::Instant;

use jarvis_model_protocol::{
    encode_frame, FrameDecoder, FrameError, FrameKind, ModelReplySender, ModelRequestReceiver,
    QueuedPlanningRequest, ReplyMailboxError, RequestMailboxError,
};

pub trait ModelTransport {
    type Error;

    /// A future implementation must use a dedicated I/O task and regularly
    /// check `request.check_live` while waiting on the owned worker. Returning
    /// a frame does not authorize its proposed action.
    fn exchange(
        &mut self,
        request_frame: &[u8],
        request: &QueuedPlanningRequest,
    ) -> Result<Vec<u8>, Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PumpOutcome {
    Idle,
    Discarded,
    ReplyQueued,
}

#[derive(Debug)]
pub enum PumpError<E> {
    RequestMailbox(RequestMailboxError),
    RequestFrame(FrameError),
    Transport(E),
    ReplyFrame(FrameError),
    ReplyMailbox(ReplyMailboxError),
}

/// One bounded work item. The monitor must never call this on its polling
/// thread: an eventual transport can wait on model I/O. Both mailbox calls
/// are nonblocking and the request token is checked before and after exchange.
pub fn pump_one<T: ModelTransport>(
    requests: &ModelRequestReceiver,
    replies: &ModelReplySender,
    transport: &mut T,
    now: Instant,
) -> Result<PumpOutcome, PumpError<T::Error>> {
    let queued = match requests.try_receive(now) {
        Ok(Some(request)) => request,
        Ok(None) => return Ok(PumpOutcome::Idle),
        Err(RequestMailboxError::Cancelled | RequestMailboxError::Expired) => {
            return Ok(PumpOutcome::Discarded);
        }
        Err(error) => return Err(PumpError::RequestMailbox(error)),
    };
    match queued.check_live(Instant::now()) {
        Ok(()) => {}
        Err(RequestMailboxError::Cancelled | RequestMailboxError::Expired) => {
            return Ok(PumpOutcome::Discarded);
        }
        Err(error) => return Err(PumpError::RequestMailbox(error)),
    }
    let frame =
        encode_frame(FrameKind::Request, queued.bytes()).map_err(PumpError::RequestFrame)?;
    let response_frame = transport
        .exchange(&frame, &queued)
        .map_err(PumpError::Transport)?;
    match queued.check_live(Instant::now()) {
        Ok(()) => {}
        Err(RequestMailboxError::Cancelled | RequestMailboxError::Expired) => {
            return Ok(PumpOutcome::Discarded);
        }
        Err(error) => return Err(PumpError::RequestMailbox(error)),
    }
    let mut decoder = FrameDecoder::new(FrameKind::Response);
    decoder
        .feed(&response_frame)
        .map_err(PumpError::ReplyFrame)?;
    let response = decoder.finish().map_err(PumpError::ReplyFrame)?;
    replies
        .try_submit(&response)
        .map_err(PumpError::ReplyMailbox)?;
    Ok(PumpOutcome::ReplyQueued)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::{RouteOutcome, Supervisor};
    use jarvis_model_protocol::{model_reply_mailbox, model_request_mailbox, ModelAdmission};
    use jarvis_profiles::TransitionCause;
    use std::path::Path;

    struct InMemoryNoAction;

    impl ModelTransport for InMemoryNoAction {
        type Error = jarvis_model_worker::FramedRequestError;

        fn exchange(
            &mut self,
            request_frame: &[u8],
            request: &QueuedPlanningRequest,
        ) -> Result<Vec<u8>, Self::Error> {
            request.check_live(Instant::now()).unwrap();
            jarvis_model_worker::handle_framed_request(request_frame)
        }
    }

    fn supervisor() -> Supervisor {
        Supervisor::bootstrap(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config")).unwrap()
    }

    #[test]
    fn in_memory_transport_reaches_only_no_action_admission() {
        let mut supervisor = supervisor();
        let (request_sender, request_receiver) = model_request_mailbox();
        let (reply_sender, reply_receiver) = model_reply_mailbox();
        assert_eq!(
            pump_one(
                &request_receiver,
                &reply_sender,
                &mut InMemoryNoAction,
                Instant::now()
            )
            .unwrap(),
            PumpOutcome::Idle
        );
        supervisor
            .queue_model_request("please open discord in a different way", &request_sender)
            .unwrap();
        assert_eq!(
            pump_one(
                &request_receiver,
                &reply_sender,
                &mut InMemoryNoAction,
                Instant::now()
            )
            .unwrap(),
            PumpOutcome::ReplyQueued
        );
        assert_eq!(
            supervisor.poll_model_reply(&reply_receiver),
            Ok(Some(ModelAdmission::NoAction))
        );
        assert!(matches!(
            supervisor.route("another unknown request"),
            Ok(RouteOutcome::Unknown)
        ));
    }

    #[test]
    fn competitive_entry_discards_queued_request_before_transport() {
        let mut supervisor = supervisor();
        let (request_sender, request_receiver) = model_request_mailbox();
        let (reply_sender, reply_receiver) = model_reply_mailbox();
        supervisor
            .queue_model_request("another unknown task", &request_sender)
            .unwrap();
        supervisor
            .enter_competitive(TransitionCause::Cs2Process)
            .unwrap();
        assert_eq!(
            pump_one(
                &request_receiver,
                &reply_sender,
                &mut InMemoryNoAction,
                Instant::now()
            )
            .unwrap(),
            PumpOutcome::Discarded
        );
        assert!(matches!(
            supervisor.poll_model_reply(&reply_receiver),
            Ok(None)
        ));
    }

    #[test]
    fn competitive_entry_during_exchange_drops_late_reply() {
        struct TransitioningTransport<'a>(&'a mut Supervisor);

        impl ModelTransport for TransitioningTransport<'_> {
            type Error = jarvis_model_worker::FramedRequestError;

            fn exchange(
                &mut self,
                request_frame: &[u8],
                _: &QueuedPlanningRequest,
            ) -> Result<Vec<u8>, Self::Error> {
                self.0
                    .enter_competitive(TransitionCause::FaceitProcess)
                    .unwrap();
                jarvis_model_worker::handle_framed_request(request_frame)
            }
        }

        let mut supervisor = supervisor();
        let (request_sender, request_receiver) = model_request_mailbox();
        let (reply_sender, reply_receiver) = model_reply_mailbox();
        supervisor
            .queue_model_request("unknown task", &request_sender)
            .unwrap();
        let result = pump_one(
            &request_receiver,
            &reply_sender,
            &mut TransitioningTransport(&mut supervisor),
            Instant::now(),
        );
        assert_eq!(result.unwrap(), PumpOutcome::Discarded);
        assert!(matches!(
            supervisor.poll_model_reply(&reply_receiver),
            Ok(None)
        ));
    }

    #[test]
    fn extra_response_frame_is_rejected_before_reply_mailbox() {
        struct ExtraResponse;

        impl ModelTransport for ExtraResponse {
            type Error = std::convert::Infallible;

            fn exchange(
                &mut self,
                _: &[u8],
                _: &QueuedPlanningRequest,
            ) -> Result<Vec<u8>, Self::Error> {
                Ok([
                    encode_frame(FrameKind::Response, b"{}").unwrap(),
                    b"extra".to_vec(),
                ]
                .concat())
            }
        }

        let mut supervisor = supervisor();
        let (request_sender, request_receiver) = model_request_mailbox();
        let (reply_sender, reply_receiver) = model_reply_mailbox();
        supervisor
            .queue_model_request("unknown task", &request_sender)
            .unwrap();
        assert!(matches!(
            pump_one(
                &request_receiver,
                &reply_sender,
                &mut ExtraResponse,
                Instant::now()
            ),
            Err(PumpError::ReplyFrame(FrameError::TrailingData))
        ));
        assert!(matches!(
            supervisor.poll_model_reply(&reply_receiver),
            Ok(None)
        ));
    }
}
