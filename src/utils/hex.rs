use crate::error::{KvError, Result};

pub fn encode(vector: &[f32]) -> String {
    let mut out = String::with_capacity(vector.len() * 8);
    for value in vector {
        for byte in value.to_le_bytes() {
            out.push_str(&format!("{byte:02x}"));
        }
    }
    out
}

pub fn decode(text: &str) -> Result<Vec<f32>> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    if text.len() % 8 != 0 {
        return Err(KvError::invalid_vector(format!(
            "vector hex length must be a multiple of 8, got {}",
            text.len()
        )));
    }
    let mut bytes = Vec::with_capacity(text.len() / 2);
    for i in (0..text.len()).step_by(2) {
        let byte = u8::from_str_radix(&text[i..i + 2], 16)
            .map_err(|_| KvError::invalid_vector("vector hex is not valid hexadecimal"))?;
        bytes.push(byte);
    }
    let mut vector = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        vector.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Ok(vector)
}

pub fn normalize(mut vector: Vec<f32>) -> Result<Vec<f32>> {
    if vector.is_empty() {
        return Err(KvError::invalid_vector("vector must not be empty"));
    }
    if vector.iter().any(|value| !value.is_finite()) {
        return Err(KvError::invalid_vector("vector contains NaN or Inf"));
    }
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in &mut vector {
            *value /= norm;
        }
    }
    Ok(vector)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let original = vec![0.25_f32, -1.5, 3.0];
        let decoded = decode(&encode(&original)).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn normalize_unit_length() {
        let vector = normalize(vec![3.0, 4.0]).unwrap();
        assert!((vector[0] - 0.6).abs() < 1e-6);
        assert!((vector[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn rejects_non_finite() {
        assert!(normalize(vec![f32::NAN]).is_err());
    }
}
