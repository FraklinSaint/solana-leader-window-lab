Inside the Block: How Solana Validators Order Transactions

by Superteam Ukraine

|

Bounty

|
Submissions Open

|

UA
4

Abhishek
Pratik
Yash

Prizes
Details
token icon
2,500
USDC
Total Prizes

800

USDC

1st

600

USDC

2nd

500

USDC

3rd

300

USDC

4th

300

USDC

5th

0

SUBMISSIONS

14d:6h:56m

REMAINING

* Costs 1 credit to submit

REGIONAL LISTING

This listing is only open for people in Ukraine

SKILLS NEEDED

Other
Backend
Blockchain
CONTACT

Reach outif you have any questions about this listing
WINNER ANNOUNCEMENT BY

November 19, 2026 - as scheduled by the sponsor.
RELATED LIVE LISTINGS

MultiHopper
MultiHopper Build & Route Challenge | Up to $50,000 + Revenue Share

MultiHopper

USDC
5,000
USDC

|

|

Due in 1mo

Daejin
Launch & Trade on Daejin: Write a Twitter Thread and Create a Market

Daejin

USDC
620
USDC

|

|

Due in 1d

La Familia
Crea Contenido para Promocionar El Encuentro 2026 en Madrid

La Familia

USDG
2,000
USDG

|

|

Due in 2d

Vaultline Security
Vaultline Security Audit Grants for Colosseum Projects: Up to 4,000 USDC Each

Vaultline Security

USDC
40k
USDC

|

|

Due in 9d

MozaikPay
Show us how you use MozaikPay

MozaikPay

USDC
500
USDC

|

|

Due in 21d

About Superteam Ukraine
Superteam Ukraine is part of the Superteam network. We're focused on supporting the Solana ecosystem and helping talented individuals connect with exciting opportunities.

Mission
Investigate and document how Solana validators order transactions within a block, specifically focusing on validators running different client software or forks.

About

he order of transactions inside a block is decided by the leader’s scheduler. In the open-source Agave code the rule is visible; how the forks that hold most of the stake behave is not publicly documented. This bounty is a measurement project: send real transactions into the leader windows of specific validators and show, from the blocks, what rule they landed by.

Scope Detail
The Question: For each validator you pick: by what rule does it order transactions in its block - what exactly decides which transaction lands earlier and which later; how strictly that rule holds; and whether there is a rule at all.

Network: All measurements must be conducted on Solana Mainnet.

Validators: Analyze at least 6 validators, ensuring they span a minimum of 3 distinct client software or fork versions (identifiable by version string or validators.app data at the time of measurement).

Fee Spend: Your code must enforce a fee spend limit. The entire measurement process should fit within approximately 0.05 SOL.

Jito Validators: Validators running Jito-Solana are in scope for analysis, but Jito bundles and tips (a separate mechanism from the leader's scheduler) are not.

Transaction Evidence: While other transactions in the block can provide supplementary data, your primary evidence must be derived from transactions you send yourself.

Experiment Design: You have full control over the experiment's design, including the type and number of transactions, how they are sent, how leader windows are detected, and how the experiment is repeated.

Considerations: Leader schedule and leader types, the path of a transaction from your backend to the node that executes it (and everything in between), the interaction of your own transactions with each other, competition inside the block, send order, noise versus rule - the README must show how your design handles each of these, or why it deliberately does not.

Alpenglow Upgrade: October 21 - the Alpenglow upgrade, take all measurements either before it or after it, do not mix.


One possible development path. Not required, but this way you do not pay for development:
- localnet (solana-test-validator) - block parsing, pipeline tests;
- devnet - an end-to-end run with real sending, for free;
- mainnet dry-run - real leader schedule and windows, transactions are built but not sent; this is where you see whether you make it into the window;
- mainnet live - with the spend cap.

The measurements themselves are mainnet only: devnet has one client and empty blocks, there is nothing to measure.

Submission Requirements
Repository: submit a link to your code repository (any language is acceptable).

Reproducibility: the project must be fully reproducible with one command, Docker Compose expected.

Keys: keys only via volume/env, never in the repo.

Execution: dry-run by default, real sending only behind an explicit flag with a spend cap.

Raw Data: include all raw data, including transaction signatures and slot numbers, to allow judges to verify claims via getBlock calls.

README: why this design, how it handles the points under “what you will have to account for”, and what it does NOT separate.

Report: report with the date of every measurement and a “what went wrong” section.

Judging Criteria
Design Validity: 35% - does the experiment measure what it claims, and are the points under “what you will have to account for”.

Verifiability and Reproducibility: 25% - The ease with which judges can verify claims using raw data and reproduce the experiment.

Analysis Quality: 25% - The depth and quality of the analysis, including honesty about noise, limitations, and null results ("No rule / it’s noise" is a valid conclusion if supported).

Engineering: 15% - The quality of the engineering, specifically regarding the dry-run functionality, spend cap implementation, logging, and containerization.

Reward Structure
1st Place: 800

2nd Place: 600

3nd Place: 500

4nd Place: 300

4nd Place: 300

