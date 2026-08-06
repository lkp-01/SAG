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
    fn protocol_exposes_epoch_ack_acceptance_and_probe_messages() {
        let register = ConnectorRegister {
            stream_epoch: "epoch-1".into(),
            capabilities: vec!["health-probe-v1".into()],
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
        let probe = HealthProbe {
            probe_id: "probe-1".into(),
            stream_epoch: register.stream_epoch.clone(),
            sent_unix_ms: 1,
        };
        let probe_ack = HealthProbeAck {
            probe_id: probe.probe_id.clone(),
            stream_epoch: probe.stream_epoch.clone(),
            received_unix_ms: 2,
        };
        assert_eq!(ack.stream_epoch, accepted.stream_epoch);
        assert_eq!(probe.stream_epoch, probe_ack.stream_epoch);
        assert!(register
            .capabilities
            .iter()
            .any(|capability| capability == "health-probe-v1"));
        assert!(matches!(
            TunnelMessage {
                payload: Some(tunnel_message::Payload::RegisterAck(ack)),
            }
            .payload,
            Some(tunnel_message::Payload::RegisterAck(_))
        ));
        assert!(matches!(
            TunnelMessage {
                payload: Some(tunnel_message::Payload::HealthProbeAck(probe_ack)),
            }
            .payload,
            Some(tunnel_message::Payload::HealthProbeAck(_))
        ));
    }
}
