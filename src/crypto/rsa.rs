// Copyright lowRISC contributors.
// Licensed under the Apache License, Version 2.0, see LICENSE for details.
// SPDX-License-Identifier: Apache-2.0

//! RSA, a public-key encryption algorithm.

#[cfg(doc)]
use std::convert::Infallible;

/// A length for the modulus of an RSA public key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ModulusLength {
    /// A 2048-bit modulus.
    Bits2048,
    /// A 3072-bit modulus.
    Bits3072,
    /// A 4096-bit modulus.
    Bits4096,
}

impl ModulusLength {
    /// Returns the number of bytes necessary to represent a modulus (or,
    /// equivalently, a ciphertext) of this size.
    pub fn byte_len(self) -> usize {
        self.bit_len() / 8
    }

    /// Returns the number of bits necessary to represent a modulus of this
    /// size.
    pub fn bit_len(self) -> usize {
        match self {
            Self::Bits2048 => 2048,
            Self::Bits3072 => 3072,
            Self::Bits4096 => 4096,
        }
    }

    /// Returns a `ModulusLength` variant corresponding to the given number of
    /// bytes, if one exists.
    pub fn from_byte_len(len: usize) -> Option<Self> {
        Self::from_bit_len(len * 8)
    }

    /// Returns a `ModulusLength` variant corresponding to the given number of
    /// bits, if one exists.
    pub fn from_bit_len(len: usize) -> Option<Self> {
        match len {
            2048 => Some(Self::Bits2048),
            3072 => Some(Self::Bits3072),
            4096 => Some(Self::Bits4096),
            _ => None,
        }
    }
}

/// The RSA public key type for a particular [`Engine`] type.
///
/// Rather than prescribe specific types of RSA keys, a particular [`Engine`]
/// implementation can provide its own key types, which implement this
/// trait.
pub trait PublicKey {
    /// Returns this key's modulus length.
    fn len(&self) -> ModulusLength;

    /// Returns true is this key is empty
    fn is_empty(&self) -> bool {
        false
    }
}

/// The RSA public/private keypair type for a particular [`Signer`] type.
///
/// This type is the keypair analogue of [`PublicKey`].
pub trait Keypair {
    /// The corresponding [`PublicKey`] implementation for this `Keypair`.
    type Pub: PublicKey;

    /// Returns a copy of the public component of this `Keypair`.
    fn public(&self) -> Self::Pub;

    /// Returns the public key's modulus length.
    fn pub_len(&self) -> ModulusLength;
}

/// An error returned by an RSA function.
///
/// This type serves as a combination of built-in error types known to
/// Manticore, plus a "custom error" component for surfacing
/// implementation-specific errors that Manticore can treat as a black box.
///
/// This type has the benefit that, unlike a pure associated type, `From`
/// implementations for error-handling can be implemented on it.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Error<E = ()> {
    /// The "custom" error type, which is treated by Manticore as a black box.
    Custom(E),
}

impl<E> Error<E> {
    /// Erases the custom error type from this `Error`, replacing it with `()`.
    pub fn erased(self) -> Error {
        match self {
            Self::Custom(_) => Error::Custom(()),
        }
    }
}

/// A builder for constructing primed RSA engines.
///
/// In particular, a value of a type implementing this trait already has
/// everything it needs (such as OS handles) to start performing RSA
/// operations.
pub trait Builder {
    /// A custom error type. If there isn't a meaningful one, use [`Infallible`].
    ///
    /// See [`Error`].
    type Engine: Engine;

    /// Checks whether [`Self::Engine`] supports public keys with moduli of
    /// length `len`. This function is primarily for `manticore` to dynamically
    /// discover all the capabilities of an engine.
    fn supports_modulus(&self, len: ModulusLength) -> bool;

    /// Creates a new [`Engine`], primed with the given key, which may be used
    /// repeatedly to perform operations.
    fn new_engine(
        &self,
        key: <Self::Engine as Engine>::Key,
    ) -> Result<Self::Engine, Error<<Self::Engine as Engine>::Error>>;
}

/// An enhanced [`Builder`] that can produce RSA signing engines.
pub trait SignerBuilder: Builder {
    /// The concrete `Signer` generated by this trait.
    type Signer: Signer<Engine = Self::Engine>;

    /// Creates a new [`Signer`], primsed with the given keypair, which may be
    /// used repeatedly to perform operations.
    fn new_signer(
        &self,
        keypair: <Self::Signer as Signer>::Keypair,
    ) -> Result<Self::Signer, Error<<Self::Engine as Engine>::Error>>;
}

/// An RSA engine, already primed with a key.
///
/// There is no way to extract the key back out of an `Engine` value.
pub trait Engine {
    /// The error returned when an RSA operation fails.
    type Error;
    /// The key type used by this engine.
    type Key: PublicKey;

    /// Uses this engine to verify `signature` against `expected_hash`, by
    /// performing an encryption operation on `signature`, and comparing the
    /// result to a hash of `message`.
    ///
    /// `signature` is expected to be in PKCS v1.5 format.
    ///
    /// If the underlying cryptographic operation succeeds, returns `Ok(())`.
    /// Failures, including signature check failures, are included in the
    /// `Err` variant.
    fn verify_signature(
        &mut self,
        signature: &[u8],
        message: &[u8],
    ) -> Result<(), Error<Self::Error>>;
}

/// An RSA signing engine, already primed with a keypair.
///
/// There is no way to extract the keypair back out of a `Signer` value.
pub trait Signer {
    /// The [`Engine`] type that this signer corresponds to.
    type Engine: Engine;

    /// The keypair type used by this signer.
    type Keypair: Keypair<Pub = <Self::Engine as Engine>::Key>;

    /// Returns the public key's modulus length (and, by extension, the length
    /// of a signature value).
    fn pub_len(&self) -> ModulusLength;

    /// Uses this signer to create a signature value for `message`.
    ///
    /// The resulting value is written to `signature`, which shall be in
    /// PKCS v1.5 format. As such, exactly `self.pub_len().byte_len()` bytes
    /// will be written to by this function.
    ///
    /// If the underlying cryptographic operation succeeds, returns `Ok(())`.
    /// Failures are included in the `Err` variant.
    fn sign(
        &mut self,
        message: &[u8],
        signature: &mut [u8],
    ) -> Result<(), Error<<Self::Engine as Engine>::Error>>;
}
