use futures::stream::StreamExt;
use libp2p::{gossipsub, mdns, noise, swarm::NetworkBehaviour, swarm::SwarmEvent, tcp, yamux};
use std::error::Error;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::{self, Duration as TokioDuration};
use tokio::{io, io::AsyncBufReadExt, select};
use tracing_subscriber::EnvFilter;

#[derive(NetworkBehaviour)]
struct MyBehaviour {
    gossipsub: gossipsub::Behaviour,
    mdns: mdns::tokio::Behaviour,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .try_init();

    let mut swarm = libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_quic()
        .with_behaviour(|key| {
            let gossipsub_config = gossipsub::ConfigBuilder::default()
                .heartbeat_interval(Duration::from_secs(10)) // This is set to aid debugging by not cluttering the log space
                // .validation_mode(gossipsub::ValidationMode::Strict) // This sets the kind of message validation. The default is Strict (enforce message signing)
                .build()
                .map_err(|msg| io::Error::new(io::ErrorKind::Other, msg))?;

            // build a gossipsub network behaviour
            let gossipsub = gossipsub::Behaviour::new(
                gossipsub::MessageAuthenticity::Signed(key.clone()),
                gossipsub_config,
            )?;

            let mdns =
                mdns::tokio::Behaviour::new(mdns::Config::default(), key.public().to_peer_id())?;
            Ok(MyBehaviour { gossipsub, mdns })
        })?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();

    let topic = gossipsub::IdentTopic::new("t-chain-test-net");
    swarm.behaviour_mut().gossipsub.subscribe(&topic)?;

    let mut stdin = io::BufReader::new(io::stdin()).lines();

    // Listen on all interfaces and whatever port the OS assigns
    swarm.listen_on("/ip4/0.0.0.0/udp/0/quic-v1".parse()?)?;
    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

    println!("Available commands: 'ADD_TRANSACTION', 'FETCH_BLOCKCHAIN'");

    #[derive(Clone, Debug)]
    struct Transaction {
        from: u64,
        to: u64,
    }
    impl Transaction {
        fn new() -> Self {
            Self {
                from: 0x000,
                to: 0x123,
            }
        }
    }

    #[derive(Clone, Debug)]
    struct Block {
        number: u64,
        transactions: Vec<Transaction>,
    }
    impl Block {
        const GENESIS_BLOCK_NUMBER: u64 = 1;

        fn new(last_mined_block_number: Option<u64>, transactions: &Vec<Transaction>) -> Self {
            Self {
                number: last_mined_block_number
                    .and_then(|last_mined_block_number| Some(last_mined_block_number + 1))
                    .unwrap_or(Self::GENESIS_BLOCK_NUMBER),
                transactions: transactions.clone(),
            }
        }
    }

    #[derive(Debug)]
    struct Blockchain {
        blocks: Mutex<Vec<Block>>,
    }

    impl Blockchain {
        const BLOCK_MINING_INTERVAL_MS: u64 = 10000;

        fn new() -> Self {
            Self {
                blocks: Mutex::new(Vec::new()),
            }
        }
        async fn get_last_mined_block_number(&self) -> Option<u64> {
            let last_mined_block = self.get_last_mined_block().await;

            last_mined_block.and_then(|block| Some(block.number))
        }
        async fn get_last_mined_block(&self) -> Option<Block> {
            let blocks = self.blocks.lock().await;

            blocks.last().cloned()
        }
        async fn mine_naively(&self, new_block: Block) {
            let mut blocks = self.blocks.lock().await;
            blocks.push(new_block);
        }
    }
    #[derive(Debug)]
    struct TransactionPool {
        transactions: Mutex<Vec<Transaction>>,
    }

    impl TransactionPool {
        fn new() -> Self {
            Self {
                transactions: Mutex::new(Vec::new()),
            }
        }
        async fn has_pending_transactions(&self) -> bool {
            self.get_transactions().await.len() > 0
        }
        async fn get_transactions(&self) -> Vec<Transaction> {
            self.transactions.lock().await.to_vec()
        }
        async fn add(&self, transaction: Transaction) {
            let mut transactions = self.transactions.lock().await;
            transactions.push(transaction)
        }
        async fn clear(&self) {
            let mut transactions = self.transactions.lock().await;
            *transactions = Vec::new();
        }
    }

    let blockchain = Arc::new(Blockchain::new());
    let transaction_pool = Arc::new(TransactionPool::new());

    {
        let blockchain = blockchain.clone();
        let transaction_pool = transaction_pool.clone();

        tokio::spawn(async move {
            let mut new_block_interval = time::interval(TokioDuration::from_millis(
                Blockchain::BLOCK_MINING_INTERVAL_MS,
            ));

            loop {
                new_block_interval.tick().await;

                let last_mined_block_number = blockchain.get_last_mined_block_number().await;

                if transaction_pool.has_pending_transactions().await {
                    let new_block = Block::new(
                        last_mined_block_number,
                        &transaction_pool.get_transactions().await,
                    );
                    blockchain.mine_naively(new_block).await;
                    transaction_pool.clear().await;
                }
            }
        });
    }

    loop {
        let transaction_pool = transaction_pool.clone();

        let add_to_transaction_pool = async move {
            transaction_pool.add(Transaction::new()).await;

            println!("Transaction Added to MemPool/TransactionPool");
        };
        select! {
            Ok(Some(line)) = stdin.next_line() => {
                match line.as_str() {
                    "ADD_TRANSACTION" => {
                        add_to_transaction_pool.await;
                        if let Err(e) = swarm.behaviour_mut().gossipsub.publish(topic.clone(), line.as_bytes()) {
                            println!("Publish error: {e:?}");
                        }
                    },
                    "FETCH_BLOCKCHAIN" =>  {
                        println!("Blockchain: {:?}", blockchain.clone());
                    }
                    _ =>  eprintln!("Invalid command")
                }

            }
            event = swarm.select_next_some() => match event {
                SwarmEvent::Behaviour(MyBehaviourEvent::Mdns(mdns::Event::Discovered(list))) => {
                    for (peer_id, _multiaddr) in list {
                        println!("mDNS discovered a new peer: {peer_id}");
                        swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                    }
                },
                SwarmEvent::Behaviour(MyBehaviourEvent::Mdns(mdns::Event::Expired(list))) => {
                    for (peer_id, _multiaddr) in list {
                        println!("mDNS discover peer has expired: {peer_id}");
                        swarm.behaviour_mut().gossipsub.remove_explicit_peer(&peer_id);
                    }
                },
                SwarmEvent::Behaviour(MyBehaviourEvent::Gossipsub(gossipsub::Event::Message {
                    propagation_source: peer_id,
                    message_id: id,
                    message,
                })) => {
                    match String::from_utf8_lossy(&message.data).as_ref() {
                        "ADD_TRANSACTION" => add_to_transaction_pool.await,
                        _ =>  eprintln!("Invalid NODE command: {} from peer: {peer_id} with id: {id}", String::from_utf8_lossy(&message.data))
                    }
                },
                SwarmEvent::NewListenAddr { address, .. } => {
                    println!("Local node is listening on {address}");
                }
                _ => {}
            }
        }
    }
}
