use std::cmp::Reverse;
use std::collections::BTreeMap;

use crate::env::{token::Token, user::UserFilter};

use super::*;
use candid::Principal;
use env::{
    config::CONFIG,
    memory,
    post::{Post, PostId},
    user::UserId,
    State,
};
use ic_cdk::{
    api::{self, call::arg_data_raw},
    caller,
};
use ic_cdk_macros::query;
use serde_bytes::ByteBuf;

#[export_name = "canister_query check_invite"]
fn check_invite() {
    let code: String = parse(&arg_data_raw());
    read(|state| reply(state.invites.contains_key(&code)))
}

#[export_name = "canister_query donors"]
fn donors() {
    read(|state| {
        let boostraping_mode =
            state.balances.values().sum::<Token>() < CONFIG.boostrapping_threshold_tokens;
        let mut donors = state
            .users
            .values()
            .map(|user| {
                (
                    user.id,
                    user.mintable_tokens(state, 1, boostraping_mode)
                        .map(|(_, tokens)| tokens)
                        .sum::<Token>(),
                )
            })
            .collect::<Vec<_>>();

        donors.sort_unstable_by_key(move |(_, tokens)| Reverse(*tokens));
        donors.truncate(100);

        reply(donors);
    });
}

#[export_name = "canister_query migration_pending"]
fn migration_pending() {
    read(|state| {
        reply(state.principal_change_requests.contains_key(&caller()));
    });
}

#[export_name = "canister_query distribution"]
fn distribution() {
    read(|state| {
        reply(&state.distribution_reports);
    });
}

#[export_name = "canister_query balances"]
fn balances() {
    read(|state| {
        reply(
            state
                .balances
                .iter()
                .map(|(acc, balance)| {
                    (
                        acc,
                        balance,
                        state
                            .principal_to_user(acc.owner)
                            .or(state.user(&acc.owner.to_string()))
                            .map(|u| u.id),
                    )
                })
                .collect::<Vec<_>>(),
        );
    });
}

#[export_name = "canister_query tokens_to_mint"]
fn tokens_to_mint() {
    read(|state| reply(state.tokens_to_mint().into_iter().collect::<Vec<_>>()))
}

#[export_name = "canister_query transaction"]
fn transaction() {
    let id: usize = parse(&arg_data_raw());
    read(|state| reply(state.ledger.get(id).ok_or("not found")));
}

#[export_name = "canister_query transactions"]
fn transactions() {
    let (page, principal, subaccount): (usize, String, String) = parse(&arg_data_raw());
    read(|state| {
        let iter = state.ledger.iter().enumerate();
        let owner = Principal::from_text(principal).expect("invalid principal");
        let subaccount = hex::decode(subaccount).expect("invalid subaccount");
        let iter: Box<dyn DoubleEndedIterator<Item = _>> = if Principal::anonymous() == owner {
            Box::new(iter)
        } else {
            Box::new(iter.filter(|(_, t)| {
                t.to.owner == owner
                    && (t.to.subaccount.is_none() || t.to.subaccount.as_ref() == Some(&subaccount))
                    || t.from.owner == owner
                        && (t.from.subaccount.is_none()
                            || t.from.subaccount.as_ref() == Some(&subaccount))
            }))
        };
        reply(
            iter.rev()
                .skip(page * CONFIG.feed_page_size)
                .take(CONFIG.feed_page_size)
                .collect::<Vec<(usize, _)>>(),
        );
    });
}

#[export_name = "canister_query proposal"]
fn proposal() {
    read(|state| {
        let id: u32 = parse(&arg_data_raw());
        reply(
            state
                .proposals
                .iter()
                .find(|proposal| proposal.id == id)
                .ok_or("no proposal found"),
        )
    })
}

#[export_name = "canister_query proposals"]
fn proposals() {
    let page_size = 10;
    let page: usize = parse(&arg_data_raw());
    read(|state| {
        reply(
            state
                .proposals
                .iter()
                .rev()
                .skip(page * page_size)
                .take(page_size)
                .filter_map(|proposal| Post::get(state, &proposal.post_id))
                .collect::<Vec<_>>(),
        )
    })
}

fn sorted_realms(
    state: &State,
    order: String,
) -> Box<dyn Iterator<Item = (&'_ String, &'_ Realm)> + '_> {
    let realm_vp = read(|state| {
        state
            .users
            .values()
            .fold(BTreeMap::default(), |mut acc, user| {
                let vp = (user.total_balance() as f32).sqrt() as u64;
                user.realms.iter().for_each(|realm_id| {
                    acc.entry(realm_id.clone())
                        .and_modify(|realm_vp| *realm_vp += vp)
                        .or_insert(vp);
                });
                acc
            })
    });
    let mut realms = state.realms.iter().collect::<Vec<_>>();
    if order != "name" {
        realms.sort_unstable_by_key(|(realm_id, realm)| match order.as_str() {
            "popularity" => {
                let realm_vp = realm_vp.get(realm_id.as_str()).copied().unwrap_or(1);
                let vp = if realm.whitelist.is_empty() {
                    realm_vp
                } else {
                    1
                };
                let moderation = if realm.filter == UserFilter::default() {
                    1
                } else {
                    realm_vp
                };
                Reverse(
                    vp * moderation
                        + (realm.num_members as f32).sqrt() as u64
                        + (realm.posts.len() as f32).sqrt() as u64,
                )
            }
            _ => Reverse(realm.last_update),
        });
    }
    Box::new(realms.into_iter())
}

#[export_name = "canister_query realms_data"]
fn realms_data() {
    read(|state| {
        let user_id = state.principal_to_user(caller()).map(|user| user.id);
        reply(
            state
                .realms
                .iter()
                .filter(|(_, realm)| realm.last_update + 2 * WEEK > time())
                .map(|(name, realm)| {
                    (
                        name,
                        (
                            &realm.label_color,
                            user_id.map(|id| realm.controllers.contains(&id)),
                            &realm.filter,
                        ),
                    )
                })
                .collect::<BTreeMap<_, _>>(),
        );
    });
}

#[export_name = "canister_query realms"]
fn realms() {
    let realm_ids: Vec<String> = parse(&arg_data_raw());
    mutate(|state| {
        reply(
            realm_ids
                .into_iter()
                .filter_map(|realm_id| {
                    state.realms.remove(&realm_id).map(|mut realm| {
                        realm.num_posts = realm.posts.len();
                        realm.posts.clear();
                        realm
                    })
                })
                .collect::<Vec<_>>(),
        )
    })
}

#[export_name = "canister_query all_realms"]
fn all_realms() {
    let page_size = 20;
    read(|state| {
        let (order, page): (String, usize) = parse(&arg_data_raw());
        reply(
            sorted_realms(state, order)
                .skip(page * page_size)
                .take(page_size)
                .collect::<Vec<_>>(),
        );
    })
}

#[export_name = "canister_query user_posts"]
fn user_posts() {
    let (handle, page, offset): (String, usize, PostId) = parse(&arg_data_raw());
    read(|state| {
        resolve_handle(state, Some(&handle)).map(|user| {
            reply(
                user.posts(state, offset, true)
                    .skip(CONFIG.feed_page_size * page)
                    .take(CONFIG.feed_page_size)
                    .collect::<Vec<_>>(),
            )
        })
    });
}

#[export_name = "canister_query rewarded_posts"]
fn rewarded_posts() {
    let (handle, page, offset): (String, usize, PostId) = parse(&arg_data_raw());
    read(|state| {
        resolve_handle(state, Some(&handle)).map(|user| {
            reply(
                user.posts(state, offset, true)
                    .filter(|post| !post.reactions.is_empty())
                    .skip(CONFIG.feed_page_size * page)
                    .take(CONFIG.feed_page_size)
                    .collect::<Vec<_>>(),
            )
        })
    });
}

#[export_name = "canister_query user_tags"]
fn user_tags() {
    let (handle, page, offset): (String, usize, PostId) = parse(&arg_data_raw());
    let tag = format!("@{}", handle);
    read(|state| {
        reply(
            state
                .last_posts(None, offset, 0, true)
                .filter(|post| post.body.contains(&tag))
                .skip(CONFIG.feed_page_size * page)
                .take(CONFIG.feed_page_size)
                .collect::<Vec<_>>(),
        )
    });
}

#[export_name = "canister_query user"]
fn user() {
    let input: Vec<String> = parse(&arg_data_raw());
    let own_profile_fetch = input.is_empty();
    mutate(|state| {
        let handle = input.into_iter().next();
        let user_id = match resolve_handle(state, handle.as_ref()) {
            Some(value) => value.id,
            _ => return reply(None as Option<User>),
        };
        let user = state.users.get_mut(&user_id).expect("user not found");
        user.num_posts = user.posts.len();
        user.posts.clear();
        if own_profile_fetch {
            user.accounting.clear();
        } else {
            user.bookmarks.clear();
            user.notifications.clear();
        }
        reply(user);
    });
}

#[export_name = "canister_query tags_cost"]
fn tags_cost() {
    let tags: Vec<String> = parse(&arg_data_raw());
    read(|state| reply(state.tags_cost(Box::new(tags.iter()))))
}

#[export_name = "canister_query invites"]
fn invites() {
    read(|state| reply(state.invites(caller())));
}

#[export_name = "canister_query posts"]
fn posts() {
    let ids: Vec<PostId> = parse(&arg_data_raw());
    read(|state| {
        reply(
            ids.into_iter()
                .filter_map(|id| Post::get(state, &id))
                .collect::<Vec<&Post>>(),
        );
    })
}

#[export_name = "canister_query journal"]
fn journal() {
    let (handle, page, offset): (String, usize, PostId) = parse(&arg_data_raw());
    read(|state| {
        reply(
            state
                .user(&handle)
                .map(|user| {
                    user.posts(state, offset, false)
                        .filter(|post| !post.body.starts_with('@'))
                        .skip(page * CONFIG.feed_page_size)
                        .take(CONFIG.feed_page_size)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        );
    })
}

#[export_name = "canister_query hot_realm_posts"]
fn hot_realm_posts() {
    let (realm, page, offset): (String, usize, PostId) = parse(&arg_data_raw());
    read(|state| {
        reply(
            state
                .hot_posts(
                    optional(realm),
                    page,
                    offset,
                    Some(&|post: &Post| post.realm.is_some()),
                )
                .collect::<Vec<_>>(),
        )
    });
}

#[export_name = "canister_query hot_posts"]
fn hot_posts() {
    let (realm, page, offset): (String, usize, PostId) = parse(&arg_data_raw());
    read(|state| {
        reply(
            state
                .hot_posts(optional(realm), page, offset, None)
                .collect::<Vec<_>>(),
        )
    });
}

#[export_name = "canister_query realms_posts"]
fn realms_posts() {
    let (page, offset): (usize, PostId) = parse(&arg_data_raw());
    read(|state| {
        let inverse_filters = state.principal_to_user(caller()).map(|user| &user.filters);
        reply(
            state
                .realms_posts(caller(), page, offset)
                .filter(|post| {
                    inverse_filters
                        .map(|filters| !post.matches_filters(filters))
                        .unwrap_or(true)
                })
                .collect::<Vec<_>>(),
        )
    });
}

#[export_name = "canister_query last_posts"]
fn last_posts() {
    let (realm, page, offset, filtered): (String, usize, PostId, bool) = parse(&arg_data_raw());
    read(|state| {
        let user = state.principal_to_user(caller());
        reply(
            state
                .last_posts(optional(realm), offset, 0, /* with_comments = */ false)
                .filter(|post| {
                    !filtered
                        || user
                            .map(|user| {
                                !post.matches_filters(&user.filters)
                                    && state
                                        .users
                                        .get(&post.user)
                                        .map(|author| user.accepts(author.id, &author.get_filter()))
                                        .unwrap_or(true)
                            })
                            .unwrap_or(true)
                })
                .skip(page * CONFIG.feed_page_size)
                .take(CONFIG.feed_page_size)
                .collect::<Vec<_>>(),
        )
    });
}

#[export_name = "canister_query posts_by_tags"]
fn posts_by_tags() {
    let (realm, tags, users, page, offset): (String, Vec<String>, Vec<UserId>, usize, PostId) =
        parse(&arg_data_raw());
    read(|state| {
        reply(
            state
                .posts_by_tags(optional(realm), tags, users, page, offset)
                .collect::<Vec<_>>(),
        )
    });
}

#[export_name = "canister_query personal_feed"]
fn personal_feed() {
    let (page, offset): (usize, PostId) = parse(&arg_data_raw());
    read(|state| {
        reply(match state.principal_to_user(caller()) {
            None => Default::default(),
            Some(user) => user.personal_feed(state, page, offset).collect::<Vec<_>>(),
        })
    });
}

#[export_name = "canister_query thread"]
fn thread() {
    let id: PostId = parse(&arg_data_raw());
    read(|state| {
        reply(
            state
                .thread(id)
                .filter_map(|id| Post::get(state, &id))
                .collect::<Vec<_>>(),
        )
    })
}

#[export_name = "canister_query validate_username"]
fn validate_username() {
    let name: String = parse(&arg_data_raw());
    read(|state| reply(state.validate_username(&name)));
}

#[export_name = "canister_query recent_tags"]
fn recent_tags() {
    let (realm, n): (String, u64) = parse(&arg_data_raw());
    read(|state| reply(state.recent_tags(optional(realm), n)));
}

#[export_name = "canister_query users"]
fn users() {
    read(|state| {
        reply(
            state
                .users
                .values()
                .map(|user| (user.id, user.name.clone(), user.rewards()))
                .collect::<Vec<(UserId, String, i64)>>(),
        )
    });
}

#[export_name = "canister_query config"]
fn config() {
    reply(CONFIG);
}

#[export_name = "canister_query logs"]
fn logs() {
    read(|state| reply(state.logs().collect::<Vec<_>>()));
}

#[export_name = "canister_query recovery_state"]
fn recovery_state() {
    read(|state| reply(state.recovery_state()));
}

#[export_name = "canister_query stats"]
fn stats() {
    read(|state| reply(state.stats(api::time())));
}

#[export_name = "canister_query search"]
fn search() {
    let query: String = parse(&arg_data_raw());
    read(|state| reply(env::search::search(state, query)));
}

#[export_name = "canister_query realm_search"]
fn realm_search() {
    let query: String = parse(&arg_data_raw());
    read(|state| reply(env::search::realm_search(state, query)));
}

#[query]
fn stable_mem_read(page: u64) -> Vec<(u64, Blob)> {
    let offset = page * BACKUP_PAGE_SIZE as u64;
    let (heap_off, heap_size) = memory::heap_address();
    let memory_end = heap_off + heap_size;
    if offset > memory_end {
        return Default::default();
    }
    let chunk_size = (BACKUP_PAGE_SIZE as u64).min(memory_end - offset) as usize;
    let mut buf = Vec::with_capacity(chunk_size);
    buf.spare_capacity_mut();
    unsafe {
        buf.set_len(chunk_size);
    }
    api::stable::stable64_read(offset, &mut buf);
    vec![(page, ByteBuf::from(buf))]
}

fn resolve_handle<'a>(state: &'a State, handle: Option<&'a String>) -> Option<&'a User> {
    match handle {
        Some(handle) => state.user(handle),
        None => Some(state.principal_to_user(caller())?),
    }
}
