use crate::keychain::*;
use pqcrypto_classicmceliece::mceliece8192128::{self, *};
use pqcrypto_falcon::falcon1024::{self, *};
use pqcrypto_traits::kem::{SharedSecret};
use hmac::{Hmac, Mac};
use sha2::Sha512;
use std::{
    str,
    fs::{self, File}, 
    path::{PathBuf, Path},
    io::{self, Cursor, Read, Write},
    env::current_dir
};

use crate::{
    ActionTypeMceliece as ActionType,
    DecryptMceliece as Decrypt,
    KeychainMceliece as Keychain, 
};
use pqcrypto_traits::sign::{
    DetachedSignature as DetachedSignatureSign, PublicKey as PublicKeySign,
    SecretKey as SecretKeySign, SignedMessage as SignedMessageSign,
};
 use crypt_guard_sign::{self, *};

#[cfg(feature = "xchacha20")]
use chacha20::{
    XChaCha20, 
    cipher::{KeyIvInit, StreamCipher, StreamCipherSeek}
};
use std::iter::repeat;
use byteorder::{BigEndian, ReadBytesExt};

#[cfg(feature = "default")]
use aes::{
    cipher::{
        self,
        BlockDecrypt, 
        generic_array::GenericArray,
        KeyInit
    },
    Aes256
};

#[cfg(feature = "dilithium")]
use crate::sign_dilithium::{self};

fn find_subarray(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

impl Decrypt {
    pub fn new() -> Self {
        Self
    }
    pub async fn generate_original_filename<'a>(&self, encrypted_path: &'a str) -> String {
       // let encrypted_path = format!("./{}", encrypted_path);
        let path = std::path::Path::new(&encrypted_path);
        let dir = path.parent().unwrap_or_else(|| std::path::Path::new(""));
        let mut file_name = path.file_stem().unwrap().to_str().unwrap().to_string();

        // Remove appended numbers and extensions like _1, _2, etc.
        if let Some(index) = file_name.rfind('_') {
            if file_name[index + 1..].chars().all(char::is_numeric) {
                file_name.truncate(index);
            }
        }

        format!("{}/{}", dir.display(), file_name)
    }

    pub fn extract_signature(signed_data: &[u8]) -> Result<(Vec<u8>, falcon1024::DetachedSignature), CryptError> {
        let mut cursor = Cursor::new(signed_data);

        // Read the length of the data
        let data_length = cursor.read_u64::<BigEndian>().unwrap() as usize;

        // Validate the length to avoid panics
        if data_length > signed_data.len() {
            return Err(CryptError::InvalidSignatureLength);
        }

        // Extract the data
        let data = signed_data[8..(8 + data_length)].to_vec();
        let signature = &signed_data[(8 + data_length)..];

        // The remaining part is the signature
        let signature: falcon1024::DetachedSignature = DetachedSignatureSign::from_bytes(&signature).unwrap();
        Ok((data, signature))
    }

    pub fn verify_signature(&self, signature: falcon1024::DetachedSignature, message: &[u8], public_key: &falcon1024::PublicKey) -> Result<bool, SigningErr> {
        // Perform the signature verification
        match falcon1024::verify_detached_signature(&signature, message, public_key) {
            Ok(_) => Ok(true),
            Err(_) => Err(SigningErr::SignatureVerificationFailed),
        }
    }




    // Function to verify the HMAC of the data
    pub fn verify_hmac(&self, key: &[u8], data_with_hmac: &[u8], hmac_len: usize) -> Result<Vec<u8>, &'static str> {
        if data_with_hmac.len() < hmac_len {
            return Err("Data is too short for HMAC verification");
        }

        let (data, hmac) = data_with_hmac.split_at(data_with_hmac.len() - hmac_len);
        let mut mac = <Hmac<Sha512> as Mac>::new_from_slice(key)
            .expect("HMAC can take key of any size");

        mac.update(data);

        if let Err(_) = mac.verify_slice(hmac) {
            eprintln!("HMAC verification failed!");
            //eprintln!("Data: {:?}", data);
            eprintln!("HMAC: {:?}", hmac);
            return Err("HMAC verification failed");
        }

        Ok(data.to_vec())
    }


    pub fn extract_encrypted_message(&self, message: &str) -> Result<Vec<u8>, CryptError> {
        let begin_tag = "-----BEGIN ENCRYPTED MESSAGE-----";
        let end_tag = "-----END ENCRYPTED MESSAGE-----";

        if let (Some(start), Some(end)) = (message.find(begin_tag), message.find(end_tag)) {
            if start < end {
                let encrypted_message = &message[start + begin_tag.len()..end].trim();
                Ok(hex::decode(encrypted_message).unwrap())
            } else {
                Err(CryptError::InvalidMessageFormat)
            }
        } else {
            Err(CryptError::MissingData)
        }
    }

    pub async fn decrypt(
        &self, 
        secret_key: PathBuf,
        ciphertext: PathBuf,
        decrypt: &str,
        action: ActionType,
        hmac_key: &[u8],
        nonce: Option<&[u8; 24]>,
    ) -> Result<(), CryptError> {
        let mut keychain = Keychain::new().unwrap();

        // Load the secret key and ciphertext
        let secret = keychain.load_secret_key(secret_key).await?;
        let cipher = keychain.load_ciphertext(ciphertext).await?;

        // Decapsulate using the secret key
        let shared_secret = decapsulate(&cipher, &secret);

        match action {
            ActionType::FileAction => {
                let path = PathBuf::from(decrypt);
                println!("Decrypting file...");

                #[cfg(feature = "default")]
                let _ = self.decrypt_file(&path, &shared_secret, hmac_key).await?;
                #[cfg(feature = "xchacha20")]
                let _ = self.decrypt_file_xchacha20(&path, &shared_secret, nonce.unwrap(), hmac_key).await?;

                Ok(())
            },
            ActionType::MessageAction => {
                println!("Decrypting message...\n");

                #[cfg(feature = "default")]
                let _ = self.decrypt_msg(decrypt.as_bytes(), &shared_secret, hmac_key, true).await?;
                #[cfg(feature = "xchacha20")]
                let _ = self.decrypt_msg_xchacha20(decrypt.as_bytes(), &shared_secret, nonce.unwrap(), hmac_key, true).await?;

                Ok(())
            },
            _ => Err(CryptError::InvalidParameters),
        }
    }
}


#[cfg(feature = "default")]
impl Decrypt {
    pub async fn decrypt_data(&self, data: &[u8], key: &[u8]) -> Result<Vec<u8>, CryptError> {
        let mut decrypted_data = vec![0u8; data.len()];
        let cipher = Aes256::new(GenericArray::from_slice(key));
        for (chunk, decrypted_chunk) in data.chunks(16).zip(decrypted_data.chunks_mut(16)) {
            let mut block = GenericArray::clone_from_slice(chunk); // Create a mutable copy
            cipher.decrypt_block(&mut block);
            decrypted_chunk.copy_from_slice(&block);
        }

        // Remove padding if present
        while decrypted_data.last() == Some(&0) {
            decrypted_data.pop();
        }

        Ok(decrypted_data)
    }

    pub async fn decrypt_file(&self, encrypted_file_path: &PathBuf, key: &dyn SharedSecret, hmac_key: &[u8]) -> Result<Vec<u8>, CryptError> {
        let decrypted_file_path = encrypted_file_path.as_os_str().to_str().ok_or(CryptError::PathError)?;
        let decrypt_file_path = self.generate_original_filename(decrypted_file_path).await;
        println!("Decrypted file path: {:?}", decrypt_file_path);

        let data = fs::read(&encrypted_file_path).map_err(|_| CryptError::IOError)?;
        let encrypted_data = self.verify_hmac(hmac_key, &data, 64).unwrap();
        let decrypted_data = self.decrypt_data(&encrypted_data, key.as_bytes()).await?;

        fs::write(&decrypt_file_path, &decrypted_data).map_err(|_| CryptError::WriteError)?;

        println!("Decryption completed and file written to {:?}", decrypt_file_path);
        Ok(decrypted_data)
    }

    pub async fn decrypt_msg(&self, encrypted_data_with_hmac: &[u8], key: &dyn SharedSecret, hmac_key: &[u8], safe: bool) -> Result<String, CryptError> {
        let encrypted_data = self.verify_hmac(hmac_key, encrypted_data_with_hmac, 64).unwrap();
        let decrypted_data = self.decrypt_data(&encrypted_data, key.as_bytes()).await?;
        let decrypted_str = String::from_utf8(decrypted_data)
            .map_err(|_| CryptError::Utf8Error)?;
        if safe {
            let message_file = fs::File::create("./message.txt");
            write!(message_file.unwrap(), "{}", &decrypted_str).unwrap();
        }
        println!("{}", &decrypted_str);
        Ok(decrypted_str)
    }
}

#[cfg(feature = "xchacha20")]
impl Decrypt {
    pub async fn decrypt_data_xchacha20(&self, encrypted_data: &[u8], nonce: &[u8; 24], key: &[u8]) -> Result<Vec<u8>, CryptError> {
        let mut decrypted_data = encrypted_data.to_vec();
        let mut cipher = XChaCha20::new(GenericArray::from_slice(key), GenericArray::from_slice(nonce));
        cipher.apply_keystream(&mut decrypted_data);

        // Remove padding if present (if you have padding)
        while decrypted_data.last() == Some(&0) {
            decrypted_data.pop();
        }

        Ok(decrypted_data)
    }

    pub async fn decrypt_file_xchacha20(&self, encrypted_file_path: &PathBuf, key: &dyn SharedSecret, nonce: &[u8; 24], hmac_key: &[u8]) -> Result<Vec<u8>, CryptError> {
        let decrypted_file_path = encrypted_file_path.as_os_str().to_str().ok_or(CryptError::PathError)?;
        let decrypt_file_path = self.generate_original_filename(decrypted_file_path).await;
        println!("Decrypted file path: {:?}", decrypt_file_path);

        let data = fs::read(&encrypted_file_path).map_err(|_| CryptError::IOError)?;

        let encrypted_data = self.verify_hmac(hmac_key, data.as_slice(), 64).unwrap();

        // Decrypt the data
        let decrypted_data = self.decrypt_data_xchacha20(&encrypted_data, &nonce, key.as_bytes()).await?;

        fs::write(&decrypt_file_path, &decrypted_data).map_err(|_| CryptError::WriteError)?;

        println!("Decryption completed and file written to {:?}", decrypt_file_path);
        Ok(decrypted_data)
    }

    pub async fn decrypt_msg_xchacha20(&self, encrypted_data_with_hmac: &[u8], key: &dyn SharedSecret, nonce: &[u8; 24], hmac_key: &[u8], safe: bool) -> Result<String, CryptError> {
        let encrypted_data = self.verify_hmac(hmac_key, encrypted_data_with_hmac, 64).unwrap();
        let decrypted_data = self.decrypt_data_xchacha20(&encrypted_data, &nonce, key.as_bytes()).await?;
        let decrypted_str = String::from_utf8(decrypted_data)
            .map_err(|_| CryptError::Utf8Error)?;
        if safe {
            let message_file = fs::File::create("./message.txt");
            write!(message_file.unwrap(), "{}", &decrypted_str).unwrap();
        }
        println!("{}", &decrypted_str);
        Ok(decrypted_str)
    }
}