//! Conservative startup validation for process-local data-plane memory bounds.

#[derive(Debug, Clone, Copy)]
pub struct MemoryBudget {
    pub budget_bytes: u64,
    pub safety_factor_percent: u8,
    pub reserved_bytes: u64,
    pub ingress_concurrency: u64,
    pub max_request_body: u64,
    pub response_concurrency: u64,
    pub max_response_body: u64,
    pub queue_capacity: u64,
    pub max_enqueued_bytes: u64,
    pub stream_capacity: u64,
    pub max_frame_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidatedMemoryBudget {
    pub required_bytes: u64,
    pub allowed_bytes: u64,
}

impl MemoryBudget {
    pub fn validate(self) -> Result<ValidatedMemoryBudget, String> {
        if self.budget_bytes == 0 {
            return Err("SAG_MEMORY_BUDGET_BYTES must be greater than zero".into());
        }
        if !(1..=100).contains(&self.safety_factor_percent) {
            return Err("memory budget safety factor must be in 1..=100 percent".into());
        }
        for (name, value) in [
            ("max_request_body", self.max_request_body),
            ("max_response_body", self.max_response_body),
            ("max_enqueued_bytes", self.max_enqueued_bytes),
            ("max_frame_bytes", self.max_frame_bytes),
        ] {
            if value == 0 || value == u64::MAX {
                return Err(format!("{name} must be finite and greater than zero"));
            }
        }
        if self.ingress_concurrency == 0 || self.response_concurrency == 0 {
            return Err("ingress and response concurrency must be greater than zero".into());
        }

        let ingress = checked_product(
            "ingress_concurrency × max_request_body",
            self.ingress_concurrency,
            self.max_request_body,
        )?;
        let responses = checked_product(
            "response_concurrency × max_response_body",
            self.response_concurrency,
            self.max_response_body,
        )?;
        let queued = checked_product(
            "queue_capacity × max_enqueued_bytes",
            self.queue_capacity,
            self.max_enqueued_bytes,
        )?;
        let streamed = checked_product(
            "stream_capacity × max_frame_bytes",
            self.stream_capacity,
            self.max_frame_bytes,
        )?;
        let required_bytes = [ingress, responses, queued, streamed]
            .into_iter()
            .try_fold(self.reserved_bytes, |total, value| {
                total
                    .checked_add(value)
                    .ok_or_else(|| "memory budget component sum overflowed u64".to_string())
            })?;
        let allowed_bytes = self
            .budget_bytes
            .checked_mul(u64::from(self.safety_factor_percent))
            .ok_or_else(|| "memory budget safety multiplication overflowed u64".to_string())?
            / 100;
        if required_bytes > allowed_bytes {
            return Err(format!(
                "configured worst-case memory {required_bytes} exceeds allowed {allowed_bytes} bytes"
            ));
        }
        Ok(ValidatedMemoryBudget {
            required_bytes,
            allowed_bytes,
        })
    }
}

fn checked_product(name: &str, left: u64, right: u64) -> Result<u64, String> {
    left.checked_mul(right)
        .ok_or_else(|| format!("memory budget component {name} overflowed u64"))
}

#[cfg(test)]
mod tests {
    use super::MemoryBudget;

    fn reasonable() -> MemoryBudget {
        MemoryBudget {
            budget_bytes: 512 * 1024 * 1024,
            safety_factor_percent: 80,
            reserved_bytes: 32 * 1024 * 1024,
            ingress_concurrency: 32,
            max_request_body: 1024 * 1024,
            response_concurrency: 32,
            max_response_body: 4 * 1024 * 1024,
            queue_capacity: 0,
            max_enqueued_bytes: 1024 * 1024,
            stream_capacity: 0,
            max_frame_bytes: 1024 * 1024,
        }
    }

    #[test]
    fn memory_budget_accepts_bounded_configuration() {
        let validated = reasonable().validate().unwrap();
        assert!(validated.required_bytes > 0);
        assert!(validated.required_bytes <= validated.allowed_bytes);
    }

    #[test]
    fn memory_budget_rejects_over_budget_zero_unbounded_and_overflow() {
        let mut config = reasonable();
        config.budget_bytes = 1;
        assert!(config.validate().is_err());

        let mut config = reasonable();
        config.max_request_body = 0;
        assert!(config.validate().is_err());

        let mut config = reasonable();
        config.max_response_body = u64::MAX;
        assert!(config.validate().is_err());

        let mut config = reasonable();
        config.ingress_concurrency = u64::MAX;
        assert!(config.validate().is_err());
    }
}
