use anchor_lang::{
    prelude::Pubkey,
    solana_program::instruction::{AccountMeta, Instruction},
    AccountDeserialize, InstructionData,
};
use litesvm::LiteSVM;
use solana_address::Address;
use solana_keypair::Keypair;
use solana_message::{Message, VersionedMessage};
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;

fn address(key: Pubkey) -> Address {
    Address::from(key.to_bytes())
}

#[test]
fn initialize_config_executes_on_sbf_and_persists_v2_defaults() {
    let program_id = address(tickersix::ID);
    let mut svm = LiteSVM::new();
    svm.add_program_from_file(
        program_id,
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../target/deploy/tickersix.so"
        ),
    )
    .unwrap();

    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 2_000_000_000).unwrap();
    let (config, _) = Pubkey::find_program_address(&[tickersix::CONFIG_SEED], &tickersix::ID);
    let instruction = Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new(address(config), false),
            AccountMeta::new_readonly(
                address(anchor_lang::solana_program::system_program::ID),
                false,
            ),
        ],
        data: tickersix::instruction::InitializeConfig {}.data(),
    };
    let message = Message::new_with_blockhash(
        &[instruction],
        Some(&payer.pubkey()),
        &svm.latest_blockhash(),
    );
    let transaction =
        VersionedTransaction::try_new(VersionedMessage::Legacy(message), &[&payer]).unwrap();
    svm.send_transaction(transaction).unwrap();

    let account = svm.get_account(&address(config)).unwrap();
    let mut data: &[u8] = &account.data;
    let config_data = tickersix::Config::try_deserialize(&mut data).unwrap();
    assert_eq!(
        config_data.admin_authority,
        Pubkey::from(payer.pubkey().to_bytes())
    );
    assert_eq!(config_data.protocol_version, tickersix::PROTOCOL_VERSION);
    assert_eq!(config_data.current_registry_version, 0);
    assert!(!config_data.registry_frozen);
}
