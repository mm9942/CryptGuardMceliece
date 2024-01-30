use pqcrypto_classicmceliece::mceliece8192128::{self, *};
use pqcrypto_traits::kem::{Ciphertext, PublicKey, SecretKey, SharedSecret};
use aes::cipher::{BlockCipher, BlockEncrypt, BlockDecrypt, KeyInit, generic_array::GenericArray};
use sha2::Sha256;
use hmac::{Hmac, Mac};
use std::{error::Error, ffi::OsStr, fmt, fs, path::Path, path::PathBuf, result::Result, env};
use tokio::runtime;
use crate::{KeychainMceliece as Keychain, FileMceliece as File};
use crypt_guard_sign::{self, *};
use pqcrypto_falcon::falcon1024;

#[derive(Debug)]
pub enum CryptError {
    IOError,
    MessageExtractionError,
    InvalidMessageFormat,
    HexError(hex::FromHexError),
    EncapsulationError,
    DecapsulationError,
    WriteError,
    HmacVerificationError,
    HmacShortData,
    HmacKeyErr,
    HexDecodingError(String),
    UniqueFilenameFailed,
    MissingSecretKey,
    MissingPublicKey,
    MissingCiphertext,
    MissingSharedSecret,
    MissingData,
    InvalidParameters,
    PathError,
    Utf8Error,
    SigningFailed,
    SignatureVerificationFailed,
    InvalidSignatureLength,
    InvalidSignature,
}

impl fmt::Display for CryptError {
   fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
       match self {
           CryptError::IOError => write!(f, "IO error occurred"),
           CryptError::MessageExtractionError => write!(f, "Error extracting message"),
           CryptError::InvalidMessageFormat => write!(f, "Invalid message format"),
           CryptError::HexError(err) => write!(f, "Hex error: {}", err),
           CryptError::EncapsulationError => write!(f, "Encapsulation error"),
           CryptError::DecapsulationError => write!(f, "Decapsulation error"),
           CryptError::WriteError => write!(f, "Write error"),
           CryptError::HmacVerificationError => write!(f, "HMAC verification error"),
           CryptError::HmacShortData => write!(f, "Data is too short for HMAC verification"),
           CryptError::HmacKeyErr => write!(f, "HMAC can take key of any size"),
           CryptError::HexDecodingError(err) => write!(f, "Hex decoding error: {}", err),
           CryptError::UniqueFilenameFailed => write!(f, "Unique filename failed"),
           CryptError::MissingSecretKey => write!(f, "Missing secret key"),
           CryptError::MissingPublicKey => write!(f, "Missing public key"),
           CryptError::MissingCiphertext => write!(f, "Missing ciphertext"),
           CryptError::MissingSharedSecret => write!(f, "Missing shared secret"),
           CryptError::MissingData => write!(f, "Missing data"),
           CryptError::InvalidParameters => write!(f, "You provided Invalid parameters"),
           CryptError::PathError => write!(f, "The provided path does not exist!"),
           CryptError::Utf8Error => write!(f, "UTF-8 conversion error"),
           CryptError::SigningFailed => write!(f, "Signing file using falcon 1024 failed!"),
           CryptError::SignatureVerificationFailed => write!(f, "verification of signature using falcon 1024 failed!"),
           CryptError::InvalidSignature => write!(f, "Signature not valid!"),
           CryptError::InvalidSignatureLength => write!(f, "Data is too short for HMAC verification"),
       }
   }
}

impl Error for CryptError {}

impl From<hex::FromHexError> for CryptError {
    fn from(error: hex::FromHexError) -> Self {
        CryptError::HexError(error)
    }
}

pub enum KeyTypes {
    All,
    PublicKey,
    SecretKey,
    SharedSecret,
    Ciphertext,
}

impl File {
    pub async fn load(path: PathBuf, file_type: KeyTypes) -> Result<Vec<u8>, CryptError> {
        let file_content = fs::read_to_string(&path).map_err(|_| CryptError::IOError)?;
        let (start_label, end_label) = match file_type {
            KeyTypes::PublicKey => ("-----BEGIN PUBLIC KEY-----\n", "\n-----END PUBLIC KEY-----"),
            KeyTypes::SecretKey => ("-----BEGIN SECRET KEY-----\n", "\n-----END SECRET KEY-----"),
            KeyTypes::SharedSecret => ("-----BEGIN SHARED SECRET-----\n", "\n-----END SHARED SECRET-----"),
            KeyTypes::Ciphertext => ("-----BEGIN CIPHERTEXT-----\n", "\n-----END CIPHERTEXT-----"),
            KeyTypes::All => unreachable!(),
        };

        let start = file_content.find(start_label)
            .ok_or(CryptError::IOError)?;
        let end = file_content.rfind(end_label)
            .ok_or(CryptError::IOError)?;

        let content = &file_content[start + start_label.len()..end];
        hex::decode(content).map_err(CryptError::HexError)
    }
}

impl Keychain {
    pub fn new() -> Result<Self, CryptError> {
        let (pk, sk) = keypair();
        let (ss, ct) = encapsulate(&pk);
        Ok(Self {
            public_key: Some(pk),
            secret_key: Some(sk),
            shared_secret: Some(ss),
            ciphertext: Some(ct),
        })
    }
    
    pub fn new_keys(path: &str, name: &str) -> Result<Self, CryptError> {
        let (pk, sk) = keypair();
        let keys = Self {
            public_key: Some(pk),
            secret_key: Some(sk),
            shared_secret: None,
            ciphertext: None,
        };
        let rt = runtime::Runtime::new().unwrap();
        rt.block_on(async {
            keys.save_keys(path, name).await;
        });
        Ok(keys)
    }
    
    pub fn find_highest_numbered_file(dir_path: &Path, base_filename: &str, extension: &str) -> Option<PathBuf> {
        let mut highest_numbered_file: Option<(i32, PathBuf)> = None;

        if dir_path.is_dir() {
            for entry in fs::read_dir(dir_path).unwrap() {
                if let Ok(entry) = entry {
                    let path = entry.path();
                    if path.is_file() && path.extension() == Some(OsStr::new(extension)) {
                        if let Some(stem) = path.file_stem().and_then(OsStr::to_str) {
                            if stem.starts_with(base_filename) {
                                let number_part = &stem[base_filename.len()..];
                                if let Ok(number) = number_part.parse::<i32>() {
                                    if highest_numbered_file.is_none() || highest_numbered_file.as_ref().unwrap().0 < number {
                                        highest_numbered_file = Some((number, path));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        highest_numbered_file.map(|(_, path)| path)
    }

    pub fn show(&self) -> Result<(), CryptError> {
        if let (Some(ref pk), Some(ref sk), Some(ref ss), Some(ref ct)) = (self.public_key.as_ref(), self.secret_key.as_ref(), self.shared_secret.as_ref(), self.ciphertext.as_ref()) {
            let ss2 = decapsulate(ct, sk);
            println!("Public Key: {}\n\nSecret Key: {}\n\nShared secret: {}\n\nDecapsulated shared secret: {}", hex::encode(pk.as_bytes()), hex::encode(sk.as_bytes()), hex::encode(ss.as_bytes()), hex::encode(ss2.as_bytes()));
            Ok(())
        } else {
            Err(CryptError::DecapsulationError)
        }
    }

    pub async fn save(&self, base_path: &str, title: &str) -> Result<(), CryptError> {
        let dir_path = format!("{}/{}", base_path, title);
        let dir = std::path::Path::new(&dir_path);
        if !dir.exists() {
            std::fs::create_dir_all(&dir).map_err(|_| CryptError::WriteError)?;
        }

        let public_key_path = Keychain::generate_unique_filename(&format!("{}/{}", dir_path, title), "pub");
        let secret_key_path = Keychain::generate_unique_filename(&format!("{}/{}", dir_path, title), "sec");
        let shared_secret_path = Keychain::generate_unique_filename(&format!("{}/{}", dir_path, title), "ss");
        let ciphertext_path = Keychain::generate_unique_filename(&format!("{}/{}", dir_path, title), "ct");

        fs::write(
            &public_key_path, 
            format!(
                "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----",
                hex::encode(self.public_key.as_ref().expect("Public key is missing").as_bytes())
            )
        ).map_err(|_| CryptError::WriteError)?;

        fs::write(
            &secret_key_path, 
            format!(
                "-----BEGIN SECRET KEY-----\n{}\n-----END SECRET KEY-----",
                hex::encode(self.secret_key.as_ref().expect("Secret key is missing").as_bytes())
            )
        ).map_err(|_| CryptError::WriteError)?;

        fs::write(
            &shared_secret_path, 
            format!(
                "-----BEGIN SHARED SECRET-----\n{}\n-----END SHARED SECRET-----",
                hex::encode(self.shared_secret.as_ref().expect("Shared secret is missing").as_bytes())
            )
        ).map_err(|_| CryptError::WriteError)?;

        fs::write(
            &ciphertext_path, 
            format!(
                "-----BEGIN CIPHERTEXT-----\n{}\n-----END CIPHERTEXT-----",
                hex::encode(self.ciphertext.as_ref().expect("Ciphertext is missing").as_bytes())
            )
        ).map_err(|_| CryptError::WriteError)?;

        Ok(())
    }

    pub async fn save_keys(&self, base_path: &str, title: &str) -> Result<(), CryptError> {
        let dir_path = format!("{}/{}", base_path, title);
        let dir = std::path::Path::new(&dir_path);
        if !dir.exists() {
            std::fs::create_dir_all(&dir).map_err(|_| CryptError::WriteError)?;
        }

        let public_key_path = Keychain::generate_unique_filename(&format!("{}/{}", dir_path, title), "pub");
        let secret_key_path = Keychain::generate_unique_filename(&format!("{}/{}", dir_path, title), "sec");

        fs::write(
            &public_key_path, 
            format!(
                "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----",
                hex::encode(self.public_key.as_ref().expect("Public key is missing").as_bytes())
            )
        ).map_err(|_| CryptError::WriteError)?;

        fs::write(
            &secret_key_path, 
            format!(
                "-----BEGIN SECRET KEY-----\n{}\n-----END SECRET KEY-----",
                hex::encode(self.secret_key.as_ref().expect("Secret key is missing").as_bytes())
            )
        ).map_err(|_| CryptError::WriteError)?;

        Ok(())
    }

    pub async fn save_public_key(&self, base_path: &str, title: &str) -> Result<(), CryptError> {
        let dir_path = format!("{}/{}", base_path, title);
        let dir = std::path::Path::new(&dir_path);
        if !dir.exists() {
            std::fs::create_dir_all(&dir).map_err(|_| CryptError::WriteError)?;
        }

        let public_key_path = Keychain::generate_unique_filename(&format!("{}/{}", dir_path, title), "pub");

        fs::write(
            &public_key_path, 
            format!(
                "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----",
                hex::encode(self.public_key.as_ref().expect("Public key is missing").as_bytes())
            )
        ).map_err(|_| CryptError::WriteError)?;

        Ok(())
    }

      
    pub async fn save_secret_key(&self, base_path: &str, title: &str) -> Result<(), CryptError> {
        let dir_path = format!("{}/{}", base_path, title);
        let dir = std::path::Path::new(&dir_path);
        if !dir.exists() {
            std::fs::create_dir_all(&dir).map_err(|_| CryptError::WriteError)?;
        }

        let secret_key_path = Keychain::generate_unique_filename(&format!("{}/{}", dir_path, title), "sec");

        fs::write(
            &secret_key_path, 
            format!(
                "-----BEGIN SECRET KEY-----\n{}\n-----END SECRET KEY-----",
                hex::encode(self.secret_key.as_ref().expect("Secret key is missing").as_bytes())
            )
        ).map_err(|_| CryptError::WriteError)?;

        Ok(())
    }


    pub async fn save_ciphertext(&self, base_path: &str, title: &str) -> Result<(), CryptError> {
        let dir_path = format!("{}/{}", base_path, title);
        let dir = std::path::Path::new(&dir_path);
        if !dir.exists() {
            std::fs::create_dir_all(&dir).map_err(|_| CryptError::WriteError)?;
        }

        let ciphertext_path = Keychain::generate_unique_filename(&format!("{}/{}", dir_path, title), "ct");

        let ciphertext = self.ciphertext.as_ref().expect("Ciphertext is missing");
        fs::write(
            &ciphertext_path, 
            format!(
                "-----BEGIN CIPHERTEXT-----\n{}\n-----END CIPHERTEXT-----",
                hex::encode(ciphertext.as_bytes())
            )
        ).map_err(|_| CryptError::WriteError)?;

        Ok(())
    }


    pub async fn save_shared_secret(&self, base_path: &str, title: &str) -> Result<(), CryptError> {
        let dir_path = format!("{}/{}", base_path, title);
        let dir = std::path::Path::new(&dir_path);
        if !dir.exists() {
            std::fs::create_dir_all(&dir).map_err(|_| CryptError::WriteError)?;
        }

        let shared_secret_path = Keychain::generate_unique_filename(&format!("{}/{}", dir_path, title), "ss");

        fs::write(
            &shared_secret_path, 
            format!(
                "-----BEGIN SHARED SECRET-----\n{}\n-----END SHARED SECRET-----",
                hex::encode(self.shared_secret.as_ref().expect("Shared secret is missing").as_bytes())
            )
        ).map_err(|_| CryptError::WriteError)?;

        Ok(())
    }


    pub async fn load_public_key(&mut self, path: PathBuf) -> Result<mceliece8192128::PublicKey, CryptError> {
        let public_key_bytes = File::load(path, KeyTypes::PublicKey).await?;
        let public_key = PublicKey::from_bytes(&public_key_bytes).unwrap();

        println!("Successfully loaded public key.\n");
        self.public_key = Some(public_key);
        Ok(public_key)
    }

    pub async fn load_secret_key(&mut self, path: PathBuf) -> Result<mceliece8192128::SecretKey, CryptError> {
        let secret_key_bytes = File::load(path, KeyTypes::SecretKey).await?;
        let secret_key: mceliece8192128::SecretKey = SecretKey::from_bytes(&secret_key_bytes).unwrap();

        println!("Successfully loaded secret key.\n");
        self.secret_key = Some(secret_key);
        Ok(secret_key)
    }

    pub async fn load_ciphertext(&mut self, path: PathBuf) -> Result<mceliece8192128::Ciphertext, CryptError> {
        let cipher_bytes = File::load(path, KeyTypes::Ciphertext).await?;
        let cipher: mceliece8192128::Ciphertext = Ciphertext::from_bytes(&cipher_bytes).unwrap();

        println!("Successfully loaded ciphertext.\n");
        self.ciphertext = Some(cipher);
        Ok(cipher)
    }

    pub async fn load_shared_secret(&mut self, path: PathBuf) -> Result<mceliece8192128::SharedSecret, CryptError> {
        let shared_secret_bytes = File::load(path, KeyTypes::SharedSecret).await?;
        let shared_secret: mceliece8192128::SharedSecret = SharedSecret::from_bytes(&shared_secret_bytes).unwrap();

        println!("Successfully loaded shared secret.\n");
        self.shared_secret = Some(shared_secret);
        Ok(shared_secret)
    }

    pub async fn get_public_key(&self) -> Result<mceliece8192128::PublicKey, CryptError> {
        let public = self.public_key.unwrap();
        Ok(public)
    }

    pub async fn get_secret_key(&self) -> Result<mceliece8192128::SecretKey, CryptError> {
        let secret = self.secret_key.unwrap();
        Ok(secret)
    }

    pub async fn get_ciphertext(&self) -> Result<mceliece8192128::Ciphertext, CryptError> {
        let cipher = self.ciphertext.unwrap();
        Ok(cipher)
    }

    pub async fn get_shared_secret(&self) -> Result<mceliece8192128::SharedSecret, CryptError> {
        let shared_sec = self.shared_secret.unwrap();
        Ok(shared_sec)
    }

    pub fn generate_unique_filename(base_path: &str, extension: &str) -> String {
        let mut counter = 1;
        let mut unique_path = format!("{}.{}", base_path, extension);
        while std::path::Path::new(&unique_path).exists() {
            unique_path = format!("{}_{}.{}", base_path, counter, extension);
            counter += 1;
        }
        unique_path
    }
}