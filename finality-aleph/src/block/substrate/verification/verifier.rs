use std::{
    fmt::{Display, Error as FmtError, Formatter},
    sync::Arc,
};

use log::debug;
use parity_scale_codec::Encode;
use sc_client_api::AuxStore;
use sc_consensus_aura::standalone::{check_equivocation, check_header_slot_and_seal, slot_author};
use sp_consensus_aura::sr25519::AuthorityPair;
use sp_consensus_slots::Slot;
use sp_runtime::{
    generic,
    traits::{Header as SubstrateHeader, Zero},
    RuntimeAppPublic,
};

use crate::{
    aleph_primitives::{AuraId, Block, Header, SessionAuthorityData, MILLISECS_PER_BLOCK},
    block::{
        self,
        substrate::{
            verification::{
                EquivocationProof, HeaderVerificationError as SubstrateHeaderVerificationError,
            },
            InnerJustification, Justification,
        },
        Header as HeaderT, HeaderVerifier, JustificationVerifier, VerifiedHeader,
    },
    crypto::AuthorityVerifier,
    justification::AlephJustification,
    session_map::AuthorityProvider,
    AuthorityId,
};

use super::VerificationError;

const LOG_TARGET: &str = "aleph-verifier";

/// A justification verifier within a single session.
#[derive(Clone, PartialEq, Debug)]
pub struct SessionVerifier {
    authority_verifier: AuthorityVerifier,
    emergency_signer: Option<AuthorityId>,
}

impl From<SessionAuthorityData> for SessionVerifier {
    fn from(authority_data: SessionAuthorityData) -> Self {
        SessionVerifier {
            authority_verifier: AuthorityVerifier::new(authority_data.authorities().to_vec()),
            emergency_signer: authority_data.emergency_finalizer().clone(),
        }
    }
}

/// Ways in which a justification can be wrong.
#[derive(Debug, PartialEq, Eq)]
pub enum SessionVerificationError {
    BadMultisignature,
    BadEmergencySignature,
    NoEmergencySigner,
}

impl Display for SessionVerificationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), FmtError> {
        use SessionVerificationError::*;
        match self {
            BadMultisignature => write!(f, "bad multisignature"),
            BadEmergencySignature => write!(f, "bad emergency signature"),
            NoEmergencySigner => write!(f, "no emergency signer defined"),
        }
    }
}

impl SessionVerifier {
    /// Verifies the correctness of a justification for supplied bytes.
    pub fn verify_bytes(
        &self,
        justification: &AlephJustification,
        bytes: Vec<u8>,
    ) -> Result<(), SessionVerificationError> {
        use AlephJustification::*;
        use SessionVerificationError::*;
        match justification {
            CommitteeMultisignature(multisignature) => {
                match self.authority_verifier.is_complete(&bytes, multisignature) {
                    true => Ok(()),
                    false => Err(BadMultisignature),
                }
            }
            EmergencySignature(signature) => match self
                .emergency_signer
                .as_ref()
                .ok_or(NoEmergencySigner)?
                .verify(&bytes, signature)
            {
                true => Ok(()),
                false => Err(BadEmergencySignature),
            },
        }
    }
}

#[derive(Debug)]
pub enum JustificationVerificationError {
    SessionVerification(SessionVerificationError),
    MissingAuthorityData,
    IncorrectGenesis,
}

impl Display for JustificationVerificationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            JustificationVerificationError::SessionVerification(e) => write!(f, "{}", e),
            JustificationVerificationError::MissingAuthorityData => {
                write!(f, "missing authority data")
            }
            JustificationVerificationError::IncorrectGenesis => {
                write!(f, "incorrect genesis header")
            }
        }
    }
}

impl From<SessionVerificationError> for JustificationVerificationError {
    fn from(value: SessionVerificationError) -> Self {
        Self::SessionVerification(value)
    }
}

pub struct Verifier<AP, H, C> {
    client: Arc<C>,
    authority_provider: AP,
    genesis_header: H,
}

impl<AP: Clone, H: Clone, C> Clone for Verifier<AP, H, C> {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            authority_provider: self.authority_provider.clone(),
            genesis_header: self.genesis_header.clone(),
        }
    }
}

impl<AP, H, C> Verifier<AP, H, C> {
    pub fn new(client: Arc<C>, authority_provider: AP, genesis_header: H) -> Self {
        Self {
            client,
            authority_provider,
            genesis_header,
        }
    }
}

impl<AP, C: AuxStore> Verifier<AP, Header, C> {
    fn check_equivocation(
        &mut self,
        slot_now: Slot,
        slot: Slot,
        header: &Header,
        expected_author: &AuraId,
    ) -> Option<block::substrate::verification::EquivocationProof>
    where
        AP: AuthorityProvider<generic::BlockId<Block>>,
    {
        match check_equivocation(
            self.client.as_ref(),
            slot_now,
            slot,
            header,
            expected_author,
        ) {
            Ok(maybe_proof) => Some(maybe_proof?.into()),
            Err(e) => {
                debug!(target: LOG_TARGET, "Error while testing for equivocation for a block: {e}");
                None
            }
        }
    }
}

impl<AP, C> JustificationVerifier<Justification> for Verifier<AP, Header, C>
where
    AP: AuthorityProvider<generic::BlockId<Block>>,
{
    type Error = JustificationVerificationError;

    fn verify_justification(
        &mut self,
        justification: Justification,
    ) -> Result<Justification, Self::Error> {
        let header = &justification.header;
        let hash = header.hash();

        match &justification.inner_justification {
            InnerJustification::Genesis => match header == &self.genesis_header {
                true => Ok(justification),
                false => Err(JustificationVerificationError::IncorrectGenesis),
            },
            InnerJustification::AlephJustification(aleph_justification) => {
                let block_id = generic::BlockId::hash(hash);
                let authority_data = self
                    .authority_provider
                    .authority_data(block_id)
                    .ok_or(JustificationVerificationError::MissingAuthorityData)?;
                let verifier = SessionVerifier::from(authority_data);
                verifier.verify_bytes(aleph_justification, header.hash().encode())?;
                Ok(justification)
            }
        }
    }
}

impl<AP, C> HeaderVerifier<Header> for Verifier<AP, Header, C>
where
    AP: AuthorityProvider<generic::BlockId<Block>>,
    C: AuxStore + Send + Sync + 'static,
{
    type Error = VerificationError;
    type EquivocationProof = EquivocationProof;

    fn verify_header(
        &mut self,
        header: <Header as HeaderT>::Unverified,
        _just_created: bool,
    ) -> Result<VerifiedHeader<Header, Self::EquivocationProof>, Self::Error> {
        // compare genesis header directly to the one we know
        if header.number().is_zero() {
            return match header == self.genesis_header {
                true => Ok(VerifiedHeader {
                    header,
                    maybe_equivocation_proof: None,
                }),
                false => Err(VerificationError::HeaderVerification(
                    SubstrateHeaderVerificationError::IncorrectGenesis,
                )),
            };
        }

        let slot_now = Slot::from_timestamp(
            sp_timestamp::Timestamp::current(),
            sp_consensus_slots::SlotDuration::from_millis(MILLISECS_PER_BLOCK),
        );
        let authorities = self
            .authority_provider
            .aura_authorities(generic::BlockId::hash(*header.parent_hash()))
            .ok_or(SubstrateHeaderVerificationError::MissingAuthorityData)?;

        let (header, slot, _) =
            check_header_slot_and_seal::<Block, AuthorityPair>(slot_now, header, &authorities)
                .map_err(|_| SubstrateHeaderVerificationError::IncorrectSeal)?;

        let expected_author = slot_author::<AuthorityPair>(slot, &authorities)
            .ok_or(SubstrateHeaderVerificationError::IncorrectAuthority)?;

        let maybe_equivocation_proof =
            self.check_equivocation(slot_now, slot, &header, expected_author);

        Ok(VerifiedHeader {
            header,
            maybe_equivocation_proof,
        })
    }

    fn own_block(&self, _header: &Header) -> bool {
        false
    }
}

#[derive(Debug)]
pub enum Either<A, B> {
    Left(A),
    Right(B),
}

impl<A: Display, B: Display> Display for Either<A, B> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Either::Left(left) => write!(f, "Either::Left: {left}"),
            Either::Right(right) => write!(f, "Either::Right: {right}"),
        }
    }
}

impl<A, B> block::EquivocationProof for Either<A, B>
where
    A: block::EquivocationProof,
    B: block::EquivocationProof,
{
    fn are_we_equivocating(&self) -> bool {
        match self {
            Either::Left(left) => left.are_we_equivocating(),
            Either::Right(right) => right.are_we_equivocating(),
        }
    }
}

#[derive(Debug)]
pub struct Pair<A, B>(A, B);

impl<A: Display, B: Display> Display for Pair<A, B> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "({}, {})", self.0, self.1)
    }
}

impl<A, B> JustificationVerifier<Justification> for (A, B)
where
    A: JustificationVerifier<Justification>,
    B: JustificationVerifier<Justification>,
{
    type Error = Pair<A::Error, B::Error>;

    fn verify_justification(
        &mut self,
        justification: <Justification as block::Justification>::Unverified,
    ) -> Result<Justification, Self::Error> {
        let (left, right) = self;
        let left_result = left.verify_justification(justification.clone());
        let err_left = match left_result {
            Ok(result) => return Ok(result),
            Err(err) => err,
        };
        let right_result = right.verify_justification(justification);
        match right_result {
            Ok(result) => {
                debug!(target: LOG_TARGET, "Justification is correct, but first verifier returned an error: {}", err_left);
                Ok(result)
            }
            Err(err_right) => Err(Pair(err_left, err_right)),
        }
    }
}

impl<A, B> HeaderVerifier<Header> for (A, B)
where
    A: HeaderVerifier<Header>,
    B: HeaderVerifier<Header>,
{
    type EquivocationProof = Either<A::EquivocationProof, B::EquivocationProof>;
    type Error = Pair<A::Error, B::Error>;

    fn verify_header(
        &mut self,
        header: <Header as HeaderT>::Unverified,
        just_created: bool,
    ) -> Result<VerifiedHeader<Header, Self::EquivocationProof>, Self::Error> {
        let (left, right) = self;
        let left_result = left.verify_header(header.clone(), just_created);
        let err_left = match left_result {
            Ok(VerifiedHeader {
                header,
                maybe_equivocation_proof,
            }) => {
                return Ok(VerifiedHeader {
                    header,
                    maybe_equivocation_proof: maybe_equivocation_proof
                        .map(|proof| Either::Left(proof)),
                })
            }
            Err(err) => err,
        };
        let right_result = right.verify_header(header, just_created);
        match right_result {
            Ok(VerifiedHeader {
                header,
                maybe_equivocation_proof,
            }) => {
                debug!(target: LOG_TARGET, "Header is correct, but first verifier returned an error: {}", err_left);
                Ok(VerifiedHeader {
                    header,
                    maybe_equivocation_proof: maybe_equivocation_proof
                        .map(|proof| Either::Right(proof)),
                })
            }
            Err(err_right) => Err(Pair(err_left, err_right)),
        }
    }

    fn own_block(&self, header: &Header) -> bool {
        self.0.own_block(header) || self.1.own_block(header)
    }
}
