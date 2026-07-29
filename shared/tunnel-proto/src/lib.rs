//! Generated gRPC types for SAG tunnel (`tunnel.proto`).
#![allow(clippy::derive_partial_eq_without_eq)]

pub mod sag {
    pub mod tunnel {
        pub mod v1 {
            tonic::include_proto!("sag.tunnel.v1");
        }
    }
}

pub use sag::tunnel::v1::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_exposes_epoch_ack_and_acceptance_messages() {
        let register = ConnectorRegister {
            stream_epoch: "epoch-1".into(),
            ..Default::default()
        };
        let request = ForwardRequest {
            stream_epoch: register.stream_epoch.clone(),
            ..Default::default()
        };
        let ack = ConnectorRegisterAck {
            stream_epoch: register.stream_epoch.clone(),
            ..Default::default()
        };
        let accepted = ForwardAccepted {
            stream_epoch: request.stream_epoch.clone(),
            ..Default::default()
        };
        assert_eq!(ack.stream_epoch, accepted.stream_epoch);
        assert!(matches!(
            TunnelMessage {
                payload: Some(tunnel_message::Payload::RegisterAck(ack)),
            }
            .payload,
            Some(tunnel_message::Payload::RegisterAck(_))
        ));
    }
}
